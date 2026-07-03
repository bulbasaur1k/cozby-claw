---
name: debug
description: Systematic debugging — reproduce, isolate, hypothesize, fix, verify. No guessing.
---

# debug

Debug by evidence, not by guessing. Weak models thrash by changing random things;
this imposes a disciplined loop that converges.

## Loop

1. **Reproduce** — get a deterministic repro (a failing test, an exact command +
   output). If you can't reproduce it, you can't fix it — gather more data first.
2. **Locate** — read the actual error/stack trace; find the exact `file:line`.
   Use `grounding` to read the real code there, not what you assume it says.
3. **Hypothesize** — state ONE specific, falsifiable cause ("X is None because Y").
4. **Test the hypothesis** — add a targeted log/assert or a minimal check that
   confirms or kills it. Do not fix yet.
5. **Fix** — make the smallest change that addresses the confirmed cause.
6. **Verify** — run the repro (and the `verify` loop). Confirm it's gone and
   nothing else broke. Remove temporary logs.

## Rules

- Change one thing at a time; revert changes that don't help.
- Never "fix" by suppressing the symptom (catch-and-ignore, widening a type) —
  address the cause you proved.
- If two hypotheses fail, widen the search (bisect the input, `git bisect`, binary-search the code path).
