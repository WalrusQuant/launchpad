# Launchpad — Foundation Audit Spec

## Purpose

We are about to integrate [launchpad-agent](https://github.com/WalrusQuant/launchpad-agent) as a native chat tab (see `specs/agent-integration-spec.md`), plus a longer queue of features behind it. Before that work starts, we want a written assessment of where the current foundation will buckle under that load.

This is a **read-only audit**. Do not edit code. Do not refactor. Produce a written report (`audits/foundation-audit-<YYYY-MM-DD>.md`) classifying each finding into one of three buckets:

- **must-fix-before-agent** — will cause user-visible breakage or expensive rework once agent integration starts
- **fix-eventually** — real issue but won't block agent work; should be tracked
- **fine-as-is** — looked at, no action needed (record so we don't re-audit later)

Every finding must cite file paths with line numbers, and where applicable a concrete reproduction or load profile that exposes the problem.

## Background the auditor needs

Read these before starting:

- `CLAUDE.md` — architecture overview, key decisions, command surface
- `specs/agent-integration-spec.md` — what's coming; informs which fragilities matter
- `CLAUDE.md` — see "Product Philosophy" and "Projects" sections for the project-window-session model (formerly `specs/project-model-spec.md`, now archived)

The agent integration introduces these new load profiles that don't exist today:

1. **Burst filesystem writes** — agent may create/edit 30+ files in <500ms during a refactor
2. **Long-running subprocess** — `lpagent` child per window, alive for hours, must be cleanly shut down on window close, must survive provider stalls
3. **Tauri event firehose** — streaming text deltas at ~50 events/second per active turn
4. **Approval-blocking turns** — agent halts waiting for user; UI must stay responsive, cancellation must work mid-block
5. **Concurrent multi-window** — two project windows, each with its own agent, both writing files
6. **Provider failures** — 429s, network drops, mid-stream errors. Need to surface clearly without crashing the chat
7. **Per-window resource ownership** — each window needs its own agent child, settings, sessions; cross-window leakage is a real risk

Score every finding against these load profiles when relevant.

---

## Audit scope — 9 areas

For each area: the question, what to investigate, what good vs bad looks like, what to record.

### 1. Event flow & state primitives

**Question:** Will the existing event bus and state propagation hold up under agent-driven burst events?

**Investigate:**
- The `fs-changed` event flow end-to-end: emitter in `src-tauri/src/lib.rs` (search for `notify` crate usage and `fs-changed` emit sites), debounce logic, frontend listener registration in `src/main.js` and `src/filebrowser.js`
- The 300ms watcher debounce — does it coalesce by path or globally? What happens to a 30-file burst?
- The 3-second git polling cycle in `src/git.js` and `src/gitpanel.js` — what's the worst-case lag between a write and the user seeing updated status?
- Active-project state in `src/projects.js` (`activeProject` module-level) — every consumer that reads it; any place that caches a stale copy
- `paneMap`, `tabMap`, `rightGroupTabIds` in `src/main.js` — how are they kept consistent? Any place a tab can exist in one map but not another?
- The `fs-changed` listener walking open editor tabs (per CLAUDE.md) — does it serialize per-tab `read_file_preview` calls? What's the cost when 20 editor tabs are open and 30 files just changed?

**Good looks like:** Event coalescing per-path, bounded refresh work per burst, single source of truth for active project, tab maps reconciled atomically.

**Bad looks like:** Global debounce that drops legitimate events, N×M reads on burst (N tabs × M changes), state read out of sync with state written, missing teardown that leaves stale entries.

**Record:** Each fragile path with a load profile that exposes it (e.g., "agent burst-writing 30 files in 200ms with 10 editor tabs open will trigger 300 read_file_preview calls in series"). Note whether the existing test suite covers any of this.

---

### 2. Cancellation & async patterns

**Question:** Is there a unified pattern for cancellable long-running ops, or will agent IPC reinvent it?

**Investigate:**
- The `GitOpSlot` pattern in `src-tauri/src/lib.rs` (per CLAUDE.md it lives there) — how `run_git_cancellable`, `spawn_git_under_slot`, `cancel_git_op` work together; the cancel-before-spawn window handling; the kill-9 choice
- The `inFlightOp` guard in `src/gitpanel.js` and how `invokeWithTimeout` calls `cancel_git_op` on timeout
- PTY backpressure: `pause_pty_reader` / `resume_pty_reader` flow
- Any other long-running Tauri commands and how they're cancelled (or aren't)
- Promise-cancellation gaps: Tauri IPC has no native cancellation — every place a JS `Promise.race` rejects without telling the Rust side to stop is a leak

**Good looks like:** A reusable cancellable-op primitive on the Rust side, a frontend wrapper that always pairs invoke + cancel, slot reservation before spawn for any process-spawning op, consistent timeout handling.

**Bad looks like:** Each subsystem (git, PTY, future agent) rolls its own cancel mechanism; orphaned Rust work after JS gives up; race windows between spawn and PID-record handled inconsistently.

**Record:** Every long-running op site, whether it's cancellable, and what would be needed to make it follow the `GitOpSlot` pattern. Recommend whether to extract a generic `CancellableOpSlot<T>` before agent code lands.

---

### 3. Error surfacing & user feedback

**Question:** Is there a uniform pattern for "something went wrong, tell the user," or will the agent layer invent its own?

**Investigate:**
- `showToast(message, type)` in `src/main.js` — call sites, types used, dismissal behavior
- `showGitFeedback` in `src/gitpanel.js` — what's panel-scoped vs app-scoped
- `showConfirmPopup` for destructive actions — call sites, positioning logic
- Rust command error returns: `Result<T, String>` is the convention per CLAUDE.md — find any commands that return `Result<T, ()>` or panic on error
- Error paths that hit `console.error` and never reach the user
- File-open errors in `createEditorTab` (per CLAUDE.md) — recently fixed to surface via toast; check if pattern is followed elsewhere
- The "tab.stale" flag — one example of a per-tab error state. Are there other places we'd want similar persistent error markers?

**Good looks like:** Three clear tiers (toast for app-level, panel feedback for panel-scoped, confirm for destructive) with consistent style; every Rust command returns a string error that's safe to display; no silent swallowing.

**Bad looks like:** Errors that only land in devtools console; mixed dialog libraries; inconsistent dismissal timing; long error strings dumped raw into toasts.

**Record:** Every error-surfacing pattern in the codebase with example call sites. Recommend a consolidated style guide section to add to `CLAUDE.md` before agent work.

---

### 4. Debug & observability

**Question:** When a user reports "the agent froze," can we figure out what happened from logs alone?

**Investigate:**
- `write_debug_log` in `src-tauri/src/lib.rs` — what calls it, what gets captured, where the log lives, rotation behavior (or lack)
- Any `eprintln!` / `println!` in Rust — these go to the .app's stderr, which is invisible to a normal user. Inventory them
- Frontend `console.log` / `console.error` — only visible in devtools. Inventory the production-relevant ones
- Tauri event names — is there a registry, or are they magic strings scattered across files?
- Timing instrumentation — anywhere we measure how long something took?
- Crash/panic handling — what happens when a Rust thread panics? Does the user see anything?

**Good looks like:** A rolling debug log with timestamps, captured both Rust and frontend events, easy to attach to a bug report. Tauri event names as constants. Production-relevant warnings reach the log, not just console.

**Bad looks like:** Debug log is opt-in and only captures one subsystem; events are stringly-typed and easy to mistype; no panic hook; no way to retroactively see what happened before a freeze.

**Record:** Current state of observability per subsystem (PTY, fs-watcher, git, settings). Recommend the minimum debug-log additions needed before agent IPC lands so a frozen turn can be diagnosed.

---

### 5. Multi-window correctness

**Question:** Do per-window resources cleanly stay per-window, with no cross-talk?

**Investigate:**
- `project_windows: HashMap<canonical_path, label>` in `AppState` — registration in `register_project_window`, cleanup in `unregister_project_window`, the lazy cleanup when `get_webview_window` returns `None`
- `focus_project_window` — race when window closes between check and focus
- Per-window state: which Tauri commands take a `tauri::Window` arg vs which assume single-window? Find any that should be window-scoped but aren't
- File watcher: one watcher per project window. What happens if the same project is opened in two windows simultaneously (should be impossible per the picker, but verify)
- Settings reload: does a settings change in window A propagate correctly to window B?
- The teardown sequence in "← Projects" — does it actually release everything, or are there orphan handles?

**Good looks like:** Every per-window resource is keyed by window label, cleanup is idempotent, lazy reconciliation handles the "closed without unregister" case.

**Bad looks like:** Global state that should be per-window (would cause two agent children to fight); cleanup that assumes orderly teardown; race between window-close and event arrival.

**Record:** Inventory of per-window resources today + any global state that the agent integration would need to make per-window. Specifically flag: where would we put the per-window agent child? Is there a clean attachment point?

---

### 6. Filesystem watcher under churn

**Question:** Does the watcher behave when 30 files change in 200ms?

**Investigate:**
- `notify` crate usage in `src-tauri/src/lib.rs` — recursive watch, debounce, event filtering
- `watch_directory` canonicalization (per CLAUDE.md) and the symlink-alias fix
- The 300ms debounce — is it per-event or batch?
- Frontend handling: `refreshFileBrowser` coalescing flag (per CLAUDE.md), editor-tab walking, git-panel refresh trigger
- What gets filtered out? `.git/`, `node_modules/`, `target/` — does the filter stay correct under burst?
- Memory behavior: does a burst of 1000 events queue up?

**Probe:** Simulate by running `mkdir -p /tmp/burst && cd /tmp/burst && for i in $(seq 1 50); do touch f$i; done` inside an open project root and watch DevTools for the resulting event volume. Document what happens.

**Good looks like:** Bounded event count delivered to frontend regardless of churn size; coalesced refresh; no UI freeze.

**Bad looks like:** Per-file event delivery without coalescing; tree rebuild per event; UI thread blocked; event queue unbounded.

**Record:** Observed behavior on the burst probe. Recommend any filtering/batching changes before the agent starts hammering the watcher.

---

### 7. CSS architecture

**Question:** Can `styles.css` (3,915 lines) cleanly absorb the agent chat tab without becoming unmaintainable?

**Investigate:**
- Section organization in `src/styles.css` — what comment-header convention, how strict is it?
- Naming conventions — BEM? loose? prefix per component?
- CSS custom properties — defined where, used consistently?
- Z-index ordering — is there a stack, or are values scattered?
- Any duplication or near-duplication across sections
- Theme handling in `src/theme.js` — how does it interact with the stylesheet?

**Estimate:** Agent chat will need ~500-1000 lines of new CSS (message bubbles, tool cards, approval cards, composer, syntax highlight inside cards). Is that absorbable here, or is now the moment to split into per-component CSS files?

**Good looks like:** Clear section discipline, named z-index tiers, custom-property-driven theming, low duplication.

**Bad looks like:** Z-index magic numbers, repeated color literals, sections out of order, no obvious place to add a new component's styles.

**Record:** Verdict on monolithic vs split. If split is recommended, propose a directory layout (`src/styles/components/`, `src/styles/layout.css`, etc.) and migration plan. If staying monolithic, propose section-discipline rules to add to `CLAUDE.md`.

---

### 8. Settings & state migration

**Question:** When we add agent settings, will the load/save path handle missing keys, version drift, and concurrent writes safely?

**Investigate:**
- `load_settings` / `save_settings` in `src-tauri/src/lib.rs` and `src/settings.js`
- Validation behavior — what happens to an unrecognized key? a missing required key? a value of the wrong type?
- Default-value handling — defaults defined where? Frontend, backend, both?
- Atomic write usage (`atomic_write` per CLAUDE.md) — confirm settings goes through it
- Per-project settings: `project-env.json` (chmod 0o600) — model to follow for `agent-config.json`?
- Migration story: what happens if we add a `agent.provider` key in v0.3 and an old user has no such key?

**Good looks like:** Tolerant load (missing keys → defaults), strict save (validation before write), atomic with mode preservation, no concurrent-write window.

**Bad looks like:** Missing-key errors that prevent app start; defaults defined in two places that drift; non-atomic settings writes; no story for adding new keys.

**Record:** Current robustness of settings load/save. Recommend any hardening needed before agent config gets added.

---

### 9. Test coverage & verification

**Question:** What's actually tested, and what gives us confidence the audit fixes don't break the existing terminal/git/editor flows?

**Investigate:**
- Run `cargo test --manifest-path src-tauri/Cargo.toml` — see what runs, note coverage areas
- Search for JS tests — Vitest? Jest? Playwright? Anything?
- CI configuration — `.github/workflows/`? What runs on PR?
- Manual-only flows — what can only be verified by clicking through the app?
- Smoke-test path — is there a documented "before merging, verify these N flows work"?

**Good looks like:** Rust unit tests for git ops (parsing, conflict detection, validation), at least integration smoke tests for filesystem ops, a documented manual smoke path, CI runs on every PR.

**Bad looks like:** Zero tests; tests that exist but aren't run in CI; no reproducible smoke path; assertions that don't actually assert.

**Record:** Honest inventory of what's tested. Recommend the minimum test scaffolding to add before agent work, given that agent integration adds async IPC, streaming, and approval flows that are hard to verify by clicking.

---

## Output format

Single markdown file at `audits/foundation-audit-<YYYY-MM-DD>.md`. Structure:

```markdown
# Foundation Audit — <date>

## Summary
- Findings: N must-fix-before-agent, M fix-eventually, K fine-as-is
- Estimated must-fix work: <X PRs, Y days>
- Top three risks if we skip the must-fix items: ...

## Methodology
- Tools used (grep, file reads, runtime probes)
- Areas covered (the 9 above)
- Areas explicitly skipped and why

## Findings

### must-fix-before-agent

#### F1. <short title>
- **Area:** <one of the 9>
- **Evidence:** `path/to/file.rs:123-145` — <quoted code or behavior description>
- **Load profile that exposes it:** <which agent load profile from the spec triggers this>
- **Why it's must-fix:** <consequence of leaving it>
- **Recommended fix:** <concrete change, sized in hours/days>

(repeat per finding)

### fix-eventually

(same structure, less detail OK)

### fine-as-is

(one-liner per area: "checked X, looked good because Y")

## Recommended PR0 scope

A concrete proposal for what should land as PR0 (pre-agent foundation work):
- Bullet list of must-fix items grouped into 1-3 PRs
- Estimated total time
- Dependencies/ordering between them

## Open questions for the human

Anything the audit couldn't resolve without product decisions.
```

---

## Conduct rules

- **Read-only.** Do not edit any source file. Do not run any command that modifies state outside `audits/` and `/tmp/`.
- **Cite evidence.** No finding without a file path + line number. No claim without a code reference or a probe result.
- **Honest unknowns.** If you can't tell whether something is fragile, write "unknown — would need <what> to verify" rather than guessing.
- **Calibrate severity.** "must-fix-before-agent" should mean "this will cause user-visible breakage or 2x rework if we don't fix it first." Don't inflate the bucket.
- **Skip the editor.** Editor improvements are explicitly out of scope. Don't audit `src/editor.js` for feature gaps.
- **Skip file-size complaints.** "This file is big" is not a finding. "This file's size hides a specific load-bearing fragility at line X" is.
- **Time-box.** Spend roughly proportional time per area; if one area is consuming disproportionate effort, note it as "needs deeper audit" and move on.

---

## Done when

- `audits/foundation-audit-<YYYY-MM-DD>.md` exists with the structure above
- Every one of the 9 areas has at least one finding (or an explicit "fine-as-is" entry)
- Summary section names a concrete PR0 scope the human can sign off on
- Total length: target 1500-3000 words. Longer means too much detail; shorter means areas got skipped.
