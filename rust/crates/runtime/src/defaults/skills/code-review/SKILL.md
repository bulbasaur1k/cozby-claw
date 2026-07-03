---
name: code-review
description: Review the current diff for correctness bugs and simplification opportunities.
---

# code-review

Review the pending changes on the current branch and report concrete, actionable findings.

## Steps

1. Establish the diff: `git diff` for the working tree, or `git diff <base>...HEAD`
   for a branch under review.
2. Read each hunk in context — open the surrounding code when a change is not
   self-explanatory.
3. Look for, in priority order:
   - **Correctness** — logic errors, off-by-one, wrong conditions, unhandled
     `None`/`Err`, race conditions, resource leaks.
   - **Security** — injection, missing validation, leaked secrets, unsafe permissions.
   - **Simplification** — dead code, duplication, needless allocations, clearer idioms.
4. Report each finding as `file:line` + one-line problem + suggested fix. Rank by
   severity. Say plainly when the diff looks clean.

## Rules

- Flag only issues you can justify from the code — no vague style nits.
- Prefer reusing existing helpers over inventing new ones in suggestions.
