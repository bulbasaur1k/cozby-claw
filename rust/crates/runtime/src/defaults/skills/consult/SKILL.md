---
name: consult
description: When stuck, escalate to the stronger external model — but only as an abstract example, never real code or secrets.
---

# consult

A weak model should not thrash forever. When you are genuinely stuck, escalate to
the configured stronger model via the `consult_external_model` tool — but do it
**safely**: this is for commercial / critical projects, so nothing proprietary
may leave the machine.

## When to escalate (do it automatically)

- The `verify` loop failed on the same error ~2 times and your fixes aren't converging.
- You cannot determine the root cause after a couple of focused attempts.
- You need a design/approach decision that's beyond the local model.

## How to escalate — abstract example ONLY

1. **Reproduce the essence generically.** Rewrite the problem as a minimal,
   self-contained example with placeholder names (`Foo`, `bar`, `T`), stripped of
   all business logic, real identifiers and data. If you can't reproduce it
   without real code, keep reducing until you can.
2. **Never include**: real project code, business rules, customer data, file
   paths, credentials, tokens, keys, emails, URLs with secrets. (The tool scans
   for these and hard-blocks the send — but don't rely on that; abstract first.)
3. Call `consult_external_model` with a conceptual `question` + the abstract
   `example`. The user reviews the exact payload before it leaves.
4. Take the general guidance back and apply it to your real code yourself.

## Rules

- If you cannot phrase the question without proprietary detail, do NOT escalate —
  solve it locally or ask the user.
- The external answer is advice on an example, not a patch for your real code —
  adapt it, then run `verify`.
