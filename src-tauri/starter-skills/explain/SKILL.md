---
name: explain
description: Explain a file, function, or piece of code — context first, then mechanics
---

The user wants to understand code. Default to "explain like a senior who hasn't seen this codebase before" — assume programming literacy, not domain knowledge.

If the user mentioned a file with `@path/to/file` or named one in their message, focus there. Otherwise, ask them which file or symbol to explain.

Structure your answer:

1. **What it does, in one sentence.** No jargon, no caveats. If you can't say it in one sentence, you don't understand it well enough yet — read more first.
2. **Why it exists.** What problem in the surrounding system does this solve that wouldn't be solved without it? Use `git log --follow path` if the *why* isn't obvious from the code.
3. **Mechanics.** Walk through the non-obvious parts. Skip lines that are self-explanatory from naming. Use `file:line` references so the user can jump to source.
4. **What to watch out for.** Sharp edges, hidden invariants, things that *look* changeable but aren't (and why).

Avoid:
- Restating the code in prose (`"the function takes x, then it does y, then it returns z"`).
- Padding with "great question" or "let me explain". Get to it.
- Inventing a *why* when the code doesn't tell you one — say "I don't see why this exists; the commit history might know".

If the code references symbols you haven't read, follow the references before explaining. Don't guess.
