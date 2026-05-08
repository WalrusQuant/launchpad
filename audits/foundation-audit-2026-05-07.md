# Foundation Audit — 2026-05-07

## Summary
- Findings: **4 must-fix-before-agent**, 9 fix-eventually, 7 fine-as-is
- Estimated must-fix work: 2 PRs, 3–4 days
- Top three risks if we skip the must-fix items:
  1. Two windows with agents running concurrent git ops will silently clobber each other's cancellation state, leaving orphaned git processes
  2. Agent burst-writing 20+ files will trigger 200MB+ of unnecessary I/O and 20+ serial IPC round-trips per refresh cycle, freezing the UI
  3. No panic hook means a single agent-runtime panic cascades through mutex poisoning to take down all PTYs, git ops, and file watchers in the process

## Methodology
- Tools: grep, file reads (View), line-number analysis across all source files
- Areas covered: all 9 areas specified in the audit spec
- Areas explicitly skipped: editor feature gaps (per spec), external library internals (launchpad-agent crate code not yet in tree)

---

## Findings

### must-fix-before-agent

#### F1. Global `git_op_pid` slot — multi-window clobber
- **Area:** 2 (Cancellation) + 5 (Multi-window)
- **Evidence:** `src-tauri/src/lib.rs:40` — `git_op_pid: Arc<Mutex<Option<GitOpSlot>>>` is a single global slot. Every window's push/pull/fetch/merge/rebase competes for the same slot.
- **Load profile:** #5 (concurrent multi-window) — two project windows each running an agent that calls `git_push` or `git_pull` will race the slot. The second window's `run_git_cancellable` overwrites the first's `GitOpSlot` (line 1917), losing the first op's `op_id` and `pid`. A subsequent `cancel_git_op` from window 1 would kill window 2's process instead.
- **Why it's must-fix:** Agent integration makes concurrent git ops common (two windows, two agents, both committing/pushing). The current design assumes at most one git network op at a time — true today because users click one button at a time, false once agents drive git.
- **Recommended fix:** Key `GitOpSlot` by window label (or connection ID). `AppState` gets `git_op_slots: Arc<Mutex<HashMap<String, GitOpSlot>>>`. Each command that calls `run_git_cancellable` takes a `tauri::Window` arg and uses `window.label()` as the key. ~4 hours.

#### F2. Editor tab scan reads ALL tabs on every `fs-changed` event — O(N×M) under burst
- **Area:** 1 (Event flow) + 6 (Watcher churn)
- **Evidence:** `src/main.js:1986–2088` — the `fs-changed` listener iterates every editor tab and calls `read_file_preview` with `maxBytes: 10485760` (10 MB) per tab. The loop is sequential (`await` in `for...of`). The watcher callback (`lib.rs:3346–3354`) discards per-file event details — it emits only the project root path, so the frontend cannot filter tabs to only those whose files changed.
- **Load profile:** #1 (burst filesystem writes) — agent refactors 30 files in <500ms. With 20 editor tabs open, this triggers 20 sequential `read_file_preview` calls (up to 10 MB each = 200 MB peak I/O) per refresh cycle. The `fsRefreshScheduled`/`fsRefreshDirty` coalescing collapses the burst to 1–2 cycles, but each cycle still reads every tab.
- **Why it's must-fix:** Agent-driven refactors routinely touch 20–50 files. The current cost is proportional to open-tab count, not change count. Users with many editor tabs will see UI freezes of multiple seconds.
- **Recommended fix:** Two changes: (a) forward changed paths from the watcher callback — `notify_debouncer_mini` provides `Vec<DebouncedEvent>` with `path` fields; emit a `Vec<String>` of changed paths in the `FsChanged` payload instead of discarding them (~1 hour in `lib.rs:3346–3354`). (b) Frontend filters editor tabs to only those whose `filePath` is in the changed set before calling `read_file_preview` (~1 hour in `main.js:1986`). Also reduce `maxBytes` from 10 MB to a smaller value (e.g., 1 MB) for change-detection reads. ~3 hours total.

#### F3. No path filtering in filesystem watcher — `.git/` and `node_modules/` churn triggers full refreshes
- **Area:** 6 (Watcher churn)
- **Evidence:** `src-tauri/src/lib.rs:3346–3354` — the watcher callback emits `fs-changed` on any non-empty event batch with zero filtering. A `git checkout`, `npm install`, or `cargo build` writes thousands of files inside `.git/`, `node_modules/`, or `target/` respectively, all triggering UI refresh cycles.
- **Load profile:** #1 (burst writes) — agent running `npm install` or `cargo build` via Bash tool produces thousands of internal churn events. Each batch triggers the full editor-tab scan (F2) plus file browser rebuild plus git panel refresh.
- **Why it's must-fix:** Agent-driven builds and dependency installs are routine. Without filtering, every `npm install` freezes the UI for seconds.
- **Recommended fix:** Filter events in the debouncer callback: skip any path containing `.git/`, `node_modules/`, `target/`, `__pycache__/`, `dist/`, `build/` (same skip list used by `search_files` at `lib.rs:992`). This requires iterating the event list instead of checking `!events.is_empty()`, but `notify_debouncer_mini` batches within 300ms so the list is manageable. ~2 hours.

#### F4. No panic hook — agent panic cascades through mutex poisoning
- **Area:** 4 (Debug & observability)
- **Evidence:** No `std::panic::set_hook()` anywhere in the codebase. PTY reader thread (`lib.rs:209`) has no panic guard. Every `Mutex::lock()` call uses `.unwrap()` (~10 occurrences, e.g., `lib.rs:1796`, `lib.rs:1866`). If any thread panics while holding a `Mutex`, all subsequent `.unwrap()` calls on that mutex will also panic (poison cascade).
- **Load profile:** #2 (long-running subprocess) — the agent runtime is async code living inside the same process. A panic in the agent's tokio tasks would poison `AppState` mutexes, taking down all PTYs, git ops, file watchers, and settings across all windows.
- **Why it's must-fix:** Agent integration adds a large body of async code (provider calls, tool execution, MCP) that will inevitably have edge-case panics during early development. Without a panic hook, one agent panic kills the entire app.
- **Recommended fix:** Add `std::panic::set_hook` in `run()` that logs the panic info to `~/.launchpad/debug.log` (or a dedicated panic log) and shows a user-facing toast. Wrap the PTY reader thread in `std::panic::catch_unwind` to prevent mutex poisoning. ~3 hours.

---

### fix-eventually

#### F5. `emit("pty-output")` broadcasts to all windows — wasteful IPC
- **Area:** 5 (Multi-window)
- **Evidence:** `lib.rs:226` — `app.emit("pty-output", ...)` sends every PTY output byte to every window. Frontend filters by `paneMap.get(tab_id)` (`main.js:1887`), which is per-window, so wrong events are harmlessly dropped.
- **Load profile:** #5 (concurrent multi-window) — each window receives N×M unnecessary serializations (N = total PTYs across all windows, M = output frequency). Not a correctness bug but wasteful.
- **Recommendation:** Use `window.emit()` instead of `app.emit()` when a window reference is available, or emit with a window-scoped event name. ~2 hours.

#### F6. Single file watcher per project path — second window steals the watcher
- **Area:** 5 (Multi-window)
- **Evidence:** `lib.rs:3373` — `watch_directory` removes the old watcher before inserting a new one for the same canonical path. If two windows open the same project, the first window stops getting `fs-changed` events. `project_windows` (`lib.rs:45`) only tracks the latest window per path (`lib.rs:3437` overwrites the mapping).
- **Load profile:** #5 (concurrent multi-window) — currently mitigated by the picker routing to the existing window via `focus_project_window`. But `open_new_window` with a `?folder=` param can bypass this.
- **Recommendation:** Use reference counting on watchers (HashMap value = `(Debouncer, u64)` count). Only drop when count reaches zero. ~3 hours.

#### F7. Tauri event names are magic strings — no constants
- **Area:** 4 (Debug & observability)
- **Evidence:** 4 Tauri events (`"pty-output"`, `"pty-exit"`, `"fs-changed"`) and 2 DOM events (`"launchpad:path-renamed"`, `"panel-transition-done"`) are bare string literals. Agent integration adds `"agent:event:<session_id>"` — more strings to mistype.
- **Recommendation:** Define shared constants in a `src/events.js` module and use them consistently. ~1 hour.

#### F8. No type validation on settings load — wrong-type values silently break runtime
- **Area:** 8 (Settings & migration)
- **Evidence:** `src/settings.js:28` — `settings = { ...settings, ...saved }` does a spread merge with no type checking. A hand-edited `config.json` with `"termFontSize": "large"` passes through to xterm.js. Agent settings add new keys that users may also hand-edit.
- **Recommendation:** Add a `validate(settings)` function that checks expected types before merge. ~2 hours.

#### F9. No schema version in settings — renamed keys have no migration path
- **Area:** 8 (Settings & migration)
- **Evidence:** No `"version"` field in `config.json`. Additive changes (new keys) work fine via spread-merge. Renamed or restructured keys silently lose user preferences.
- **Recommendation:** Add a `"version": 1` field. Before merge, check version and run migration functions. ~2 hours when needed.

#### F10. Settings changes don't propagate to other open windows
- **Area:** 5 (Multi-window)
- **Evidence:** `src/settings.js:35` — `saveSetting` updates in-memory `settings` and writes to disk. No event emitted. Other windows keep stale in-memory copy until rebooted. Agent settings (provider, API key) changed in one window won't take effect in the other.
- **Recommendation:** Emit a `settings-changed` Tauri event from `save_settings`. Other windows listen and reload. ~2 hours.

#### F11. Long-running commands without cancellation — `search_in_files`, `git_resolve_*`, stash ops
- **Area:** 2 (Cancellation)
- **Evidence:** `search_in_files` (`lib.rs:1022–1139`) walks the entire project tree reading files up to 2 MB each. `git_resolve_ours/theirs` (`lib.rs:2997–3036`) spawns shell git without PID tracking. `git_stash_pop` (`lib.rs:3131–3137`) uses libgit2 directly. None are cancellable.
- **Load profile:** #2 (long-running subprocess) — agent calling `search_in_files` on a large monorepo, or `git_resolve_*` during conflict resolution, could block the Tauri command thread for 10+ seconds.
- **Recommendation:** Wrap spawn-based commands in `run_git_cancellable`. For libgit2 commands, add periodic `cancelled` flag checks. For `search_in_files`, add a row-count limit and periodic check. ~4 hours.

#### F12. CSS accent colors are hardcoded, not custom properties — theme gaps
- **Area:** 7 (CSS architecture)
- **Evidence:** `src/styles.css` lines 1–65 define `--surface-*`, `--border-*`, `--text-*` custom properties for dark/light themes. Accent colors (`#57c7ff` blue ~30 instances, `#5af78e` green ~20, `#ff5f56` red ~15) are hardcoded throughout. Agent chat tab will add ~500–1000 more lines with more accent usage.
- **Recommendation:** Extract accent colors to custom properties (`--accent-blue`, `--accent-green`, `--accent-red`, etc.) before adding agent CSS. ~2 hours.

#### F13. Zero frontend tests — no test infrastructure
- **Area:** 9 (Test coverage)
- **Evidence:** No `*.test.js`, no Vitest/Jest/Playwright config, no test runner in `package.json`. Rust has ~65 unit tests with good coverage (git ops, file I/O, validation, state machines), CI runs `cargo test` only on macOS.
- **Recommendation:** Add Vitest with at least smoke tests for settings load/save, project state, and event handling before agent UI work begins. Not blocking but high-value. ~1 day to set up + write initial tests.

---

### fine-as-is

#### A1. Active-project state (`activeProject`) — single source of truth, no stale cache
- `src/projects.js:3` — module-level `let activeProject = null`, set once at `enterWorkspace`, cleared at teardown. All consumers null-check before reading. Session-long singleton, not a cache.

#### A2. `paneMap`/`tabs`/`rightGroupTabIds` consistency — no cross-map desync possible
- JS is single-threaded; all mutations are synchronous. Cleanup removes from data structures before destroying resources. No inconsistency window.

#### A3. 300ms watcher debounce — correct batch coalescing
- `notify_debouncer_mini` batches all events within 300ms into one callback. The Launchpad callback emits one `fs-changed` per batch. Frontend `fsRefreshScheduled`/`fsRefreshDirty` provides a second coalescing layer. A 30-file burst in <300ms produces exactly one refresh cycle.

#### A4. Git polling — snapshot dedup prevents redundant DOM work
- `gitpanel.js:150–151` — JSON.stringify comparison skips re-render when data unchanged. `inFlightOp` guard prevents toolbar re-render mid-op. Clean design.

#### A5. `refreshFileBrowser` coalescing — double-guarded against concurrent DOM mutation
- `filebrowser.js:578–595` — `refreshInFlight` flag + `refreshDirty` loop, plus `loadDirectory` serializes top-level loads via promise chain. No flicker, no concurrent tree builds.

#### A6. Settings atomic write — confirmed robust
- `lib.rs:660` routes through `atomic_write` (defined `lib.rs:1216`), which creates temp file with PID + counter suffix, fsyncs, preserves mode, then renames. No crash-window for data loss. `project-env.json` uses `atomic_write_with_mode(path, data, Some(0o600))` — chmod on temp before rename, no 0o644 exposure.

#### A7. Rust command error returns — consistent `Result<T, String>`
- Every Tauri command returns `Result<_, String>` with `.map_err(|e| e.to_string())`. No `Result<T, ()>`, no panics in command handlers. Only two `.expect()` calls — one is an invariant guarantee (UTF-8), one is the Tauri builder.

---

## Recommended PR0 scope (auditor's original proposal — see "Final PR0 scope" below for the executable version)

### PR0a — Multi-window safety + watcher filtering (2–3 days)

| Item | Effort | Blocks |
|------|--------|--------|
| F1: Per-window `GitOpSlot` | 4h | Agent git ops |
| F3: Watcher path filtering | 2h | Agent burst writes |
| F2: Forward changed paths + filter editor tabs | 3h | Agent burst writes |
| F4: Panic hook + PTY reader `catch_unwind` | 3h | Agent runtime stability |

**Ordering:** F3 and F2 can be done in parallel. F1 is independent. F4 is independent but should land first since it protects against panics introduced by the other changes.

### PR0b — Observability + event hygiene (1 day, can overlap with PR0a)

| Item | Effort | Blocks |
|------|--------|--------|
| F7: Event name constants | 1h | Agent events |
| F12: Accent color custom properties | 2h | Agent chat CSS |

PR0b is lower priority and can slip to PR1 phase if needed.

**Total estimated PR0 time: 3–4 days.**

---

## Resolved questions

1. **Per-window vs per-process git ops.** **Resolved: per-window.** Global-queue serialization would block window B's status check on window A's push — bad UX, and the per-window model maps cleanly to how agents will use git (each window's agent owns its own git operations). F1's recommended fix stands as written.

2. **Watcher filtering granularity.** **Resolved: hardcoded list for v1.** Use the same skip list as `search_files` at `lib.rs:992` (`.git/`, `node_modules/`, `target/`, `__pycache__/`, `dist/`, `build/`). A `.launchpadignore` file is a feature, not a foundation requirement — revisit only if a user explicitly asks.

3. **Frontend test investment.** **Resolved: defer.** Don't add Vitest in PR0. Land it alongside the chat tab PR (PR2 of agent integration) when there's actually testable UI worth covering. Smoke tests of settings load/save in isolation are low ROI.

4. **`read_file_preview` maxBytes for change detection.** **Resolved: 1 MB cap for change-detection reads.** Keep the 10 MB limit for explicit file-open (the existing `createEditorTab` path). Add a separate smaller cap for the `fs-changed` re-read pass in F2. Files larger than 1 MB simply won't auto-refresh on external change — acceptable, since multi-MB files aren't first-class editor citizens anyway.

## Adjustments to PR0 scope (per human review)

- **F11 (other long-running commands without cancellation):** Stays in fix-eventually. `search_in_files` cancellation matters once the agent uses it for context-gathering, but does not block agent PR1.
- **F12 (accent color custom properties):** Move out of PR0b. Land it as part of the first PR that writes agent chat CSS (PR2 of agent integration), so the extraction happens in context.
- **F13 (Vitest):** Move out of PR0. Land alongside PR2 of agent integration per resolved question 3.

This collapses **PR0b** to just F7 (event name constants — 1h). At that size, fold it into PR0a as a single PR rather than carrying a separate PR0b.

### Final PR0 scope

| Item | Effort | Order |
|------|--------|-------|
| F4: Panic hook + PTY reader `catch_unwind` | 3h | First (protects subsequent changes) |
| F1: Per-window `GitOpSlot` | 4h | Second |
| F3: Watcher path filtering (hardcoded skip list) | 2h | Parallel with F2 |
| F2: Forward changed paths + filter editor tabs (1 MB cap on change-detection reads) | 3h | Parallel with F3 |
| F7: Event name constants in `src/events.js` | 1h | Last (mechanical cleanup) |

**Total: ~13 hours of focused work, single PR.**
