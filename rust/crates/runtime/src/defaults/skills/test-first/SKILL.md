---
name: test-first
description: Test-driven development — write a failing test, make it pass, refactor. Red/green/refactor.
---

# test-first

Drive the change with a test. For a weak model this externalizes "what does done
mean" into an executable check, and gives the `verify` loop something concrete to
turn green.

## Loop

1. **Red** — write the smallest test that captures the desired behavior. Run it;
   confirm it fails for the right reason (not a typo/compile error).
2. **Green** — write the minimum code to make that test pass. Nothing extra.
3. **Refactor** — clean up while the test stays green (see `refactor`).
4. Repeat for the next behavior. Keep each cycle small.

## Rules

- One behavior per test; name it after the behavior, not the function.
- Assert on observable outcomes, not internal implementation details.
- Don't write code with no failing test demanding it (no speculative features).
- Match the project's existing test framework and layout — read a neighboring test first.
