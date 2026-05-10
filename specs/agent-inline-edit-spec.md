# Launchpad — Inline Agent Edit Spec

> **Status — 2026-05-09:** Design draft. Not started. Lands as v0.4.x feature
> after v0.3 (chat tab) has had a few weeks of real-world dogfooding.

## Context

Launchpad's chat tab is the conversational surface — open a tab, type a prompt,
get a response with tool cards. Great for "explain this", "plan this", or any
multi-turn back-and-forth.

But a huge chunk of how people actually use coding agents is: **point at code
in an editor, type a one-line instruction, get a focused edit, accept or
reject.** That's an *inline* surface — the conversation is one-shot, the
context is the selection, the output is a diff overlay. Cursor's Cmd+K, Copilot
inline chat, and Zed's assistant all converged on this pattern because it
removes the tab-switch + copy-paste tax that chat tabs impose.

We already shipped 90% of the runtime machinery for this. The agent integration
(spec: `agent-integration-spec.md`) gave us:

- An in-process agent runtime that can take structured input and produce
  patches via `apply_patch`
- The `diffparse.js` parser that handles unified-diff format
- The `diffrender.js` renderer that draws structured diffs
- An ephemeral session model (`session/start` already accepts `ephemeral: true`)
- Per-window event routing via `agent:event:<window_label>`

What's new is **a UI surface inside the CodeMirror editor**: a floating
composer anchored to a selection, an inline diff preview that visually
overlays the proposed change, and Accept / Reject affordances.

This spec is the design for that surface.

---

## 1. Goals & non-goals

### Goals

- One keypress in an editor → composer ready to take an instruction
- One Send → focused diff appears inline, in-place, with the original code
  visible alongside it
- One keypress → Accept (apply diff) or Reject (dismiss)
- Total round-trip under 3 seconds for small edits with a fast model
- Reuse the existing agent runtime — no new transport, no new session model
- Tight tool surface: the inline-edit agent gets `read` and produces a patch;
  it does **not** run shell commands, write files outside the active tab's
  file, or call workspace tools (`lp_open_in_editor`, etc.)

### Non-goals (v1)

- **Multi-file edits.** The inline agent only modifies the file in the active
  editor tab. If the model returns a patch that touches other files, we reject
  the whole patch with a "use the chat tab for multi-file edits" toast.
- **Conversation continuation.** One-shot. No "now also rename the function" —
  the user invokes again.
- **Inline edit in diff / merge / rebase tabs.** Only regular editor tabs.
- **Streaming partial-diff rendering.** v1 shows a "working…" indicator, then
  swaps to the full diff on completion. Streaming partial diffs is a polish
  follow-up.
- **Undo across the inline edit boundary.** When the user accepts, the change
  goes through CodeMirror's normal `dispatch`, so undo works the standard way.
  We don't add a separate "undo last inline edit" stack.
- **Inline edit history sidebar.** Inline-edit sessions are ephemeral and
  filtered out of the persisted-sessions list. They're not conversations.

---

## 2. UX flow

```
┌─────────────────────────────────────────────────────┐
│  function calculateTotal(items) {                  │   ← cursor here, or
│  ┌─────────────────────────────────────────────┐   │     selection covers
│  │ ✦  add error handling for null items     [→]│   │     the function body
│  │  Esc to cancel                              │   │   ← composer widget
│  └─────────────────────────────────────────────┘   │     (inline, pushes
│    return items.reduce((s, i) => s + i.price, 0); │     content down)
│  }                                                  │
└─────────────────────────────────────────────────────┘
                     ↓ user types instruction, hits Enter

┌─────────────────────────────────────────────────────┐
│  function calculateTotal(items) {                  │
│  ┌─────────────────────────────────────────────┐   │
│  │ ✦  ⠋ working…                            [✕]│   │   ← composer becomes
│  └─────────────────────────────────────────────┘   │     a status row
│    return items.reduce((s, i) => s + i.price, 0); │
│  }                                                  │
└─────────────────────────────────────────────────────┘
                     ↓ agent returns a patch

┌─────────────────────────────────────────────────────┐
│  function calculateTotal(items) {                  │
│  ┌─────────────────────────────────────────────┐   │
│  │ ✦  proposed edit         [Accept ⏎] [Reject]│   │   ← diff actions
│  └─────────────────────────────────────────────┘   │
│-   return items.reduce((s, i) => s + i.price, 0); │   ← red strikethrough
│+   if (!Array.isArray(items)) return 0;            │   ← green new lines
│+   return items.reduce(                            │     (CodeMirror
│+     (s, i) => s + (i?.price ?? 0),                │      decorations)
│+     0                                             │
│+   );                                              │
│  }                                                  │
└─────────────────────────────────────────────────────┘
                     ↓ user hits ⏎

         (patch applies, decorations clear, composer dismisses)
```

### State machine

```
  ┌──────┐  invoke  ┌──────────┐ submit ┌─────────┐  patch  ┌────────────┐
  │ idle │ ───────→ │ composer │ ─────→ │ working │ ──────→ │  preview   │
  └──────┘          └──────────┘        └─────────┘         └────────────┘
                          │                  │                    │
                          │ Esc              │ ✕ cancel           │ Accept ⏎
                          │                  │                    │
                          ▼                  ▼                    ▼
                       ┌──────────────────────────────┐    ┌──────────────┐
                       │           dismiss            │    │ apply patch, │
                       │  (interrupt turn if running) │    │   dismiss    │
                       └──────────────────────────────┘    └──────────────┘
                                                                   │
                                                                   ▼
                                                                ┌──────┐
                                                                │ idle │
                                                                └──────┘
                                              (Reject button → dismiss path too)
```

### Trigger

**Open question — see § 11.** Cursor uses `Cmd+K` (collides with our terminal
"Clear screen"). Copilot uses `Cmd+I` (collides with our "New agent chat
tab"). Candidates: `Cmd+E`, `Cmd+;`, `Cmd+Shift+I`, `Cmd+Option+E`.

### Selection vs cursor

Both are valid:

- **Selection present** → the selected range is "the code to transform"
- **Cursor only** → "insert here" mode. The agent gets the whole function /
  block the cursor is in (CodeMirror's syntax tree can give us the enclosing
  scope) plus the cursor position as the insertion target.

For v1, simplest path: if there's a selection, use it. Otherwise use the
current line plus N lines on either side (configurable, default ±25).

### Anchoring

The composer is a CodeMirror `WidgetType` decoration inserted at the line
**below** the selection's last line (or below the cursor's line for
cursor-only invocations). It pushes the rest of the document down — same
pattern as VS Code's "Find" widget when it's anchored to a line.

Why inline (push content down) instead of floating overlay:

- Doesn't obscure code the user might want to reference while typing
- Resizes naturally with the editor's font / wrap settings
- Survives editor scroll without re-positioning math
- CodeMirror handles the layout — we don't fight against it

---

## 3. Architecture

### Core insight: lean on what already exists

We do NOT need:
- A new transport (existing per-window connection works)
- A new session type (existing `ephemeral: true` flag works)
- A new tool (the agent uses the same `apply_patch` it already knows)
- New runtime plumbing (zero changes in `crates/`)

We DO need:
- A frontend module that orchestrates the inline-edit lifecycle
- A CodeMirror extension for the composer + diff overlay decorations
- A bundled "inline-edit" skill (system prompt) that constrains the model
- Maybe a thin Tauri command if we want to bypass the chat-tab event router

### Two backend approaches

#### Option A: Reuse `agent_send_message`, route events to inline UI (preferred)

Frontend invokes:

1. `agent_session_start({ project_path, ephemeral: true })` — gets a session_id
   that's marked ephemeral so it doesn't persist in `RolloutStore`
2. `agent_send_message(session_id, prompt, [], ["inline-edit"])` — the
   `inline-edit` skill is bundled (see § 5) and instructs the model to output
   a patch in a fenced code block in its assistant message
3. Listens on the existing `agent:event:<window_label>` channel; the
   inline-edit controller filters by session_id (just like the chat tab does)
4. On `turn/completed`, parses the assistant message text for a fenced
   `\`\`\`diff … \`\`\`` block
5. Renders the parsed patch as inline decorations
6. After Accept/Reject, the controller calls `agent_session_close` (TODO: does
   this command exist? if not, ephemeral sessions auto-clean on next reload)

**Why this is preferred:** zero backend changes. The whole feature lives in
the frontend + a bundled skill + an upstream tweak to make ephemeral sessions
actually drop after the turn (currently `ephemeral` only stops persistence,
the runtime still keeps the in-memory entry until reload).

#### Option B: New `agent_inline_edit` Tauri command (rejected for v1)

Synchronous command that builds the session, runs the turn, parses the
patch, and returns it. Throws away streaming, requires backend code, and
doesn't add real value over option A. Document it as an option if option A
hits friction.

### File layout

```
src/inlineedit/
  inline-edit.js          # Public API: openInlineEdit(view, range)
                          # Owns the controller per active edit. One at a
                          # time per editor tab.
  inline-composer.js      # CodeMirror WidgetType for the composer:
                          # textarea + send button + ✕ cancel + Esc handler
  inline-controller.js    # State machine (idle / composer / working /
                          # preview), agent session lifecycle, event
                          # filtering, patch parsing, accept/reject actions
  inline-diff.js          # CodeMirror StateField + Decorations for the
                          # inline diff overlay (red strikethrough on old,
                          # green widget lines on new)

src-tauri/starter-skills/inline-edit/
  SKILL.md                # Bundled skill: "you produce exactly one diff
                          # in a fenced code block, nothing else"

# Optional, only if Option A's session-cleanup path needs it:
src-tauri/src/agent/commands.rs
  agent_session_close     # Tauri command to drop an ephemeral session
                          # immediately. Currently we just let them die on
                          # reload.
```

### Editor wiring

The new module is wired in `src/main.js::createEditorTab` — when an editor tab
is created, register the inline-edit CodeMirror extension and bind the
trigger keystroke to `openInlineEdit(view, view.state.selection.main)`.

The extension owns:

- The keymap for the trigger key (Cmd+E or whatever lands)
- A `StateField` tracking the active inline edit's status + decorations
- A `WidgetType` for the composer
- A `WidgetType` for the diff preview's "ghost" inserted lines

---

## 4. Frontend state machine

```javascript
// inline-controller.js
{
  status: "idle" | "composer" | "working" | "preview" | "applying",
  view: EditorView,                  // CodeMirror view (the editor)
  range: { from: number, to: number }, // original selection / cursor range
  contextRange: { from, to },        // expanded ±N lines for context
  composerEl: HTMLElement,           // the active composer DOM
  sessionId: string | null,
  turnId: string | null,
  proposedPatch: { hunks, ... } | null,
  unsubscribe: () => void,           // detaches the agent event listener
}
```

Transitions:

- **idle → composer** — `openInlineEdit(view, range)`. Build the composer
  widget, insert it as a decoration below the selection, focus the textarea.
- **composer → working** — user submits. Composer hides its input, shows
  spinner + Cancel. Backend `agent_session_start({ ephemeral: true })` then
  `agent_send_message(sessionId, prompt, [], ["inline-edit"])`. Subscribe to
  agent events filtered by sessionId.
- **working → preview** — `turn/completed` arrives. Parse the assistant
  message for a fenced diff block via `diffparse.js`. Validate it touches
  only the active file (reject + toast if not). Render decorations. Show
  Accept / Reject buttons in the composer row.
- **working → idle (cancel)** — user clicks ✕ or hits Esc. Send
  `agent_interrupt_turn`. Tear down composer + listener.
- **preview → applying → idle** — Accept. Run the patch through CodeMirror's
  `view.dispatch({ changes })` so it goes through the normal undo history.
  Tear down composer + decorations + listener.
- **preview → idle (reject)** — Reject. Tear down composer + decorations +
  listener. Document untouched.

---

## 5. The `inline-edit` skill (bundled)

A new starter skill at `src-tauri/starter-skills/inline-edit/SKILL.md`,
seeded into `~/.lpagent/skills/inline-edit/SKILL.md` on first run alongside
the existing four. The skill body is the system instructions for inline-edit
mode — strict rules optimized for small focused diffs.

```markdown
---
name: inline-edit
description: Produce a focused, ready-to-apply unified-diff patch — used by
  Launchpad's inline editor invocation
---

You are operating in **inline-edit mode**. The user selected a region of
code in their editor and gave you a one-line instruction. Your only job is
to produce a focused diff that does what they asked.

Output rules:
- Reply with exactly ONE fenced `diff` code block, in unified-diff format.
- The diff must touch only the file the user is currently editing. Do not
  propose changes to any other file.
- Do not call any tools. Do not run shell commands. Do not read other files.
  The user gave you the code; you give them back a patch.
- Do not include prose before or after the code block — the UI extracts the
  patch from the block and discards everything else.
- Keep the patch minimal. Don't reformat unrelated lines, don't fix unrelated
  issues. The user can invoke you again for those.
- If the request is ambiguous or impossible, output a single comment line
  inside the fenced block instead of a diff:
      ```diff
      # cannot do this: <one-sentence reason>
      ```

Format the patch as:

```diff
*** Begin Patch
*** Update File: path/to/file.ext
@@
 unchanged context line
-line to remove
+line to add
 more unchanged context
*** End Patch
```

(Use the standard `*** Begin Patch / *** Update File / @@ / *** End Patch`
envelope `apply_patch` accepts. Do NOT use `git diff` headers, `--- a/` /
`+++ b/` markers, or index hashes — those will be rejected.)
```

This skill is purely prompt-based — it doesn't restrict tool calls at the
runtime level. If models cheat and call other tools anyway, we ignore those
events; only the assistant message text is parsed for the patch. (Future
hardening: pass an `allowed_tools` hint at session creation to harden this
at the runtime level.)

---

## 6. Inline diff rendering (CodeMirror decorations)

Three decoration kinds composed:

1. **Removed-line decoration** (`Decoration.mark`) — applied to each line
   inside the original selection that the patch removes. CSS: red background
   tint, strikethrough text. Stays in the document until Accept/Reject.

2. **Inserted-line widgets** (`Decoration.widget` with `block: true`) —
   inserted as block widgets between lines. Each widget renders one or more
   added lines with green background, plus-prefix indentation. They sit
   visually below the corresponding removed line so add+remove pairs read as
   a side-by-side diff in vertical space.

3. **Header widget** — a slim row at the top of the diff zone showing
   "proposed edit · Accept ⏎ · Reject Esc · file_path · +X / -Y". Acts as
   the persistent affordance for keyboard accept/reject after the user
   clicks back into the editor.

On Accept, all three decoration kinds are removed and a single
`view.dispatch({ changes: ... })` applies the actual text change. The user's
undo stack now has one undoable inline-edit entry.

On Reject, all three decoration kinds are removed and the document is left
exactly as it was.

### Why CodeMirror decorations and not a separate diff pane

- Same screen real estate, no context switch
- Original code stays visible — user can scan the surrounding context while
  evaluating the diff
- No layout shift when the diff arrives (the widgets push content down by a
  predictable amount, same as the composer)
- We already have `diffparse.js` producing structured hunks; mapping hunk
  lines to CodeMirror decorations is a tight loop, not a rendering rewrite

---

## 7. Settings & preferences

New section in the settings panel under Agent → "Inline edit":

- **Enabled** — boolean, default true. Off → the keybinding does nothing.
- **Trigger keystroke** — dropdown of valid combinations (TBD per § 11).
  Default whichever shortcut we land on.
- **Context window** — number of lines on each side of the cursor / selection
  to include as context. Default 25.
- **Accept-by-tab** — boolean, default false. When true, `Tab` accepts the
  preview the same way `Enter` does. Off by default because Tab also has
  meaning inside the composer textarea.

All inline-edit settings persist to `~/.launchpad/config.json` like other
settings. None are project-scoped (matches the rest of the panel).

---

## 8. Cross-feature interaction

| Surface | Interaction |
|---------|-------------|
| Editor `fs-changed` listener | If the file changes externally during a working-state turn, cancel the inline edit and toast "file changed externally during edit". The proposed patch may no longer apply cleanly. |
| Editor save (Cmd+S) | Disabled while in `composer` / `working` / `preview` state — the document is mid-edit. Re-enabled after Accept/Reject. |
| Other editor tabs | Inline edit is per-view. Switching to another editor tab while a preview is showing leaves the preview in place; switching back resumes it. |
| Chat tab | Independent. Inline edits don't appear in any chat tab's session list (ephemeral flag). The runtime is the same; the surfaces are separate. |
| Cmd+W on the editor tab | If an inline edit is in `working` or `preview` state, prompt the user the same way unsaved-changes does. Cancel-and-close discards the proposed patch. |
| Conflict mode editors | Inline edit is disabled (the conflict UI owns the editor). |

---

## 9. Error handling

| Case | Behavior |
|------|----------|
| No agent provider configured | Disable the keybinding; on invocation, show the same empty-state CTA the chat tab uses. |
| Network / model error mid-turn | Composer status row shows "edit failed: <error>" with Retry / Dismiss buttons. |
| Patch parse failure | Show "model returned an unparseable patch — try rephrasing" with the raw response viewable behind a "show response" toggle. |
| Patch touches files other than the active file | Reject the whole patch with "use the chat tab for multi-file edits". |
| Patch's hunk lines don't actually match the document (drifted while waiting) | Show "patch no longer applies cleanly — file changed during edit" + Retry button. |
| Model output exceeds N kB without producing a fenced diff block | Cancel turn at N kB, show "model rambled — try a more focused instruction". |

---

## 10. Phased PR plan

### PR1 — Skill + dispatch plumbing (no UI)

- Add `src-tauri/starter-skills/inline-edit/SKILL.md` and register it in
  `STARTER_SKILLS` (host.rs). The seeder will pick it up on next run.
- Verify end-to-end via the existing chat tab: create a new chat, type
  `/inline-edit ` then a code snippet + instruction, confirm the model
  outputs a fenced diff block.
- (Optional, only if needed for cleanup) `agent_session_close` Tauri command
  to drop ephemeral sessions explicitly.

**Done when:** the bundled skill produces parseable patches reliably across
Anthropic, OpenAI, and Z.ai models in chat-tab smoke tests.

### PR2 — Composer widget (UI shell, no agent calls)

- New `src/inlineedit/` module with `inline-composer.js` and stub
  `inline-controller.js`.
- CodeMirror extension wired into editor tabs that:
  - Binds the trigger keystroke
  - Inserts the composer widget below the selection
  - Captures input + Submit / Cancel actions, but logs them instead of
    calling the agent
- Settings panel "Inline edit" section with Enabled toggle and trigger
  keystroke dropdown.

**Done when:** pressing the trigger in any editor tab opens a composer;
typing + Submit logs the prompt to console; Esc dismisses.

### PR3 — Agent invocation + working state

- Wire the controller's submit path to `agent_session_start` (ephemeral) +
  `agent_send_message` (with the inline-edit skill).
- Subscribe to the per-window agent event channel; filter by sessionId.
- Working-state UI: composer collapses to spinner + Cancel.
- Cancel sends `agent_interrupt_turn` and dismisses.
- On `turn/completed`, parse the assistant text for a fenced diff block via
  `diffparse.js`; for now just `console.log` the parsed hunks.

**Done when:** end-to-end submit produces a parsed hunk array in the console
within a few seconds.

### PR4 — Inline diff preview + Accept/Reject

- `inline-diff.js` builds the three decoration kinds (removed-line,
  inserted-line widget, header widget).
- Controller transitions `working → preview` on parse success.
- Accept dispatches a CodeMirror change set built from the parsed hunks,
  goes through normal undo history.
- Reject removes all decorations.
- Validation: reject patches that touch other files, or whose hunks don't
  match the current document.

**Done when:** end-to-end inline edit works on a real file, Accept applies
correctly, undo undoes the inline edit as one entry.

### PR5 — Polish

- Streaming partial-diff rendering (parse the assistant text incrementally
  on `item.delta` and update decorations as the patch grows)
- Accept-by-tab setting
- "Show raw response" toggle for parse failures
- Per-language hint passed to the model (the file extension → "you are
  editing a Rust file")
- Telemetry hooks (count successful accepts, reject rate, parse failure
  rate) — internal only, not phoned home

---

## 11. Open questions

1. **Trigger keystroke.** Cmd+K conflicts with terminal Clear, Cmd+I
   conflicts with our existing "new agent tab". Options: Cmd+E, Cmd+;,
   Cmd+Shift+I, Cmd+Option+E. Configurable in settings either way; we just
   need a sensible default. **Lean: Cmd+E** — short, easy to type, no
   collision in the current keymap.
2. **Cursor-only invocation context.** When there's no selection, do we send
   the cursor's enclosing function/block (requires CodeMirror syntax tree
   walking) or just ±N lines? Lean: ±N lines for v1, syntax-tree-aware in
   PR5 polish.
3. **Should the chat tab show inline-edit history?** The user might want to
   see "what edits did the agent make today" somewhere. Lean: no for v1 —
   inline edits are micro-interactions, not conversations. Could surface a
   "recent inline edits" view in settings if there's demand.
4. **Multi-file edits — graceful degradation or hard reject?** Lean: hard
   reject in v1 with a "use the chat tab for that" toast. The chat tab
   handles multi-file naturally; we don't want two ways to do the same
   thing.
5. **Re-prompt on parse failure?** When the model returns prose without a
   fenced diff, do we auto-retry with a stricter prompt, or just surface
   the failure? Lean: surface the failure for v1; auto-retry hides costs
   from the user.
6. **Inline edit in the chat composer itself?** Recursion gets weird. Lean:
   no.

---

## 12. Risks

- **Rendering flicker.** CodeMirror block widgets cause a layout shift when
  inserted. If the composer + diff sequence isn't smooth, the feature feels
  janky. Need to time the animation carefully; possibly add a CSS transition
  on the widget's height.
- **Patch matching drift.** Between the user submitting and the agent
  returning, the user could keep typing in the editor (unless we lock it).
  If the patch's context lines no longer match, we have to detect that and
  surface a useful error, not crash. Currently leaning toward soft-locking
  the editor (read-only) during `working` state.
- **Streaming model behavior.** Some providers (particularly local Ollama
  models) take 10+ seconds for a small edit. The `working…` indicator needs
  to feel alive (animated dot is enough). If it's too slow, users will go
  back to Cmd+P → manual edit.
- **Skill drift across models.** The inline-edit skill works great on
  Claude/GPT-4 in tests; we don't know how it behaves on glm-4.6, llama,
  qwen, etc. Need to dogfood across providers and possibly ship
  per-provider skill variants in PR5 polish.
- **Tool registry leak.** The skill says "do not call any tools," but a
  badly-aligned model might call `bash` or `read` anyway. Those calls
  succeed in the runtime and run for real. Mitigation in PR5: pass an
  `allowed_tools` hint at session creation to harden this at the runtime
  level (requires a small upstream change to the lpa server).
- **`ephemeral: true` doesn't actually drop the in-memory session.** Need
  to verify; if it doesn't, every inline edit leaks a session entry until
  the next runtime reload. May need an `agent_session_close` command.

---

## 13. Out of scope (decisions of record)

Don't propose these without strong new evidence:

- **Multi-file inline edits.** Use the chat tab. The whole point of inline
  is "this small piece of code right here."
- **Streaming character-by-character into the document.** Some tools (like
  Continue) stream the new code directly into the document as it generates.
  Cute demo, terrible UX — text appears, then half-disappears, then
  reappears as the model corrects itself. Patch-then-preview is the right
  model.
- **Per-project inline-edit skill overrides.** Could be useful, adds config
  surface. Defer until someone asks.
- **An inline-edit history sidebar.** See open question #3.
- **A "thinking pane" showing the model's reasoning during the edit.** The
  whole point of inline is brevity. If users want to see the agent's
  reasoning, they should use the chat tab.

---

## 14. Implementation cost estimate

- PR1 (skill + smoke test): 1 day
- PR2 (composer UI shell): 2 days
- PR3 (agent invocation + working state): 2 days
- PR4 (diff preview + Accept/Reject): 3-4 days
- PR5 (polish): 3-5 days

Total: ~2 weeks of focused work to ship v1. The UX polish in PR4-5 is the
long pole; the agent integration is essentially free given the chat tab
already exists.

---

## Appendix: prompt-shape comparison

For reference, this is roughly the message we'd construct for an inline-edit
turn (with the inline-edit skill prepended automatically by the runtime):

```
[skill: inline-edit]

I'm editing this file: src/auth.ts
Language: TypeScript

Context (lines 40-90, the selection is lines 55-72):

  40 | export interface AuthOptions {
  41 |   apiKey: string;
  42 |   ...
  ...
  55 | export async function authenticate(opts: AuthOptions) {        ← selection start
  56 |   const res = await fetch(opts.endpoint, {
  57 |     method: 'POST',
  58 |     ...
  ...
  72 |   return data.token;
  73 | }                                                                ← selection end
  74 |
  ...
  90 | }

Selection (lines 55-72):
  [the actual selected text, verbatim, no line numbers]

Instruction: add error handling for non-200 responses
```

The model returns:

````
```diff
*** Begin Patch
*** Update File: src/auth.ts
@@
 export async function authenticate(opts: AuthOptions) {
   const res = await fetch(opts.endpoint, {
     method: 'POST',
     ...
   });
+  if (!res.ok) {
+    throw new AuthError(`auth failed: ${res.status} ${res.statusText}`);
+  }
   const data = await res.json();
   return data.token;
 }
*** End Patch
```
````

`diffparse.js` already handles this format. The frontend extracts the fenced
block, parses it, validates it touches `src/auth.ts` only, and renders the
diff inline.
