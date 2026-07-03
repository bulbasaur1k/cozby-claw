---
name: plan
description: Turn a goal into a small, ordered, verifiable task list before touching code. Plan-then-execute.
---

# plan

Separate planning from execution. Break the goal into small steps a weak model
can do one at a time, each with an explicit verification. The task list is your
external working memory — keep it live.

## Steps

1. Restate the goal in one sentence and list the constraints (from CLAUDE.md, the
   task, and what you found in the code).
2. Explore just enough to plan: locate the files/functions involved (use the
   `grounding` skill). Do not edit yet.
3. Write the plan as an ordered list of **2–5 minute tasks**. Each task states:
   - the exact file(s)/symbol(s) to change,
   - what changes,
   - how it will be **verified** (which `verify` check or test proves it).
4. Flag risks and the rollback point.
5. Execute one task at a time; after each, run its verification before the next.

## Rules

- Prefer many small, independently verifiable steps over one big change.
- If a step turns out bigger than ~5 minutes, split it.
- Do not batch unverified edits across many files — that hides which one broke.
- Update the list as you learn; mark tasks done as you finish them.
