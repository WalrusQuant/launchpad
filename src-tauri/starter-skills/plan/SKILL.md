---
name: plan
description: Break a feature request into a phased implementation plan with concrete steps
---

The user wants a plan, not code. Resist the urge to start writing code — your job in this turn is to think clearly enough that the work is obvious afterward.

Process:

1. **Restate the goal in one sentence.** If you can't, ask one clarifying question — the most load-bearing one — and stop.
2. **Survey the relevant code** before planning. Use Glob, Grep, Read on the files that will be touched. A plan written without reading the code is fiction.
3. **List constraints you discovered**: APIs that already exist, decisions of record (look for `CLAUDE.md`, `specs/`, recent commits), things that *can't* change.
4. **Write the plan as numbered phases.** Each phase should be:
   - Independently committable (each phase leaves the tree in a working state)
   - Small enough to review in one sitting (≤ 4 files touched, ≤ ~200 lines diff)
   - Stated as the *outcome*, not the *task* (`"Editor tabs reload on external rename"`, not `"add a watcher"`)
5. **Within each phase**, list the concrete steps: files to touch, functions to add/modify, tests to write. Reference real `file:line` for anchors.
6. **Call out unknowns explicitly.** "I don't know how X is wired — need to check Y first" beats a confident-but-wrong plan.
7. **End with a "what could go wrong" section.** Top 2-3 risks and what to look for.

Don't:
- Pad with "first we'll do X, then we'll do Y" boilerplate.
- Plan for hypothetical future requirements ("we might want to support…").
- Estimate time. You don't know.
