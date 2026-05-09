---
name: commit
description: Generate a conventional commit message from the current staged changes
---

You are about to write a git commit message. Follow this protocol:

1. Run `git status` and `git diff --staged` (via the Bash tool) to see what is actually staged. Do NOT include unstaged changes.
2. If nothing is staged, stop and tell the user there are no staged changes.
3. Read the diff and decide:
   - **type**: `feat` (new user-facing capability), `fix` (bug fix), `refactor` (no behavior change), `docs`, `test`, `chore`, `perf`, `style`, `build`, `ci`. Use `feat` only when it's wholly new — improving an existing feature is `fix` or `refactor` depending on intent.
   - **scope** (optional): a single noun for the affected subsystem, lowercase, no spaces (e.g. `auth`, `git-panel`, `composer`).
   - **subject**: imperative mood, present tense, no trailing period, ≤ 70 chars. Focus on the *why* over the *what* when the diff makes the *what* obvious.
4. Compose the commit message. Format:
   ```
   <type>(<scope>): <subject>
   ```
   Add a body paragraph (wrapped at ~72 chars) only when the change is non-obvious or has a hidden constraint worth recording.
5. Show the proposed message and **ask the user to confirm** before running `git commit`. Do not commit without explicit approval.
6. If approved, run `git commit -m "<message>"` (use a heredoc for multiline). Then run `git status` to confirm.

Never:
- Skip pre-commit hooks (`--no-verify`) unless the user explicitly asks.
- Amend a previous commit unless the user explicitly asks.
- Include sensitive files (`.env`, credentials) — refuse and warn if they are staged.
