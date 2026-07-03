---
name: refactor
description: Change structure without changing behavior, verifying at every step.
---

# refactor

Improve code structure while keeping behavior identical, proven by tests at each
step. Never mix a refactor with a behavior change in the same step.

## Steps

1. Ensure there is a green safety net first: run the tests (`verify`). If the area
   has no tests, add characterization tests before refactoring.
2. Make ONE small structural change (extract function, rename, dedupe, inline).
3. Run the tests / `verify` immediately. Green → keep; red → revert and try smaller.
4. Repeat. Commit-sized chunks stay behavior-preserving.

## Rules

- No behavior changes and no refactor in the same step — separate them.
- Match the surrounding code's naming and idioms; don't introduce a new style.
- Prefer reusing existing helpers over creating parallel ones.
- Stop when the code is clear enough — don't gold-plate.
