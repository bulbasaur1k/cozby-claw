---
name: commit
description: Craft a clean git commit for the current changes — conventional style, no noise.
---

# commit

Review the working tree and prepare a well-formed git commit for the current changes.

## Steps

1. Inspect what changed: `git status`, `git diff`, and `git diff --staged`.
2. Decide the scope: one logical commit, or flag that it should be split.
3. Write the message:
   - **Subject** — `type(scope): summary`, imperative mood, ≤ 72 chars.
     Types: `feat`, `fix`, `refactor`, `docs`, `test`, `chore`, `perf`, `build`.
   - **Body** (optional) — the *what* and *why*, wrapped near 72 columns.
     Skip it when the subject already says everything.
4. Show the proposed message and the exact `git commit` command.

## Rules

- Do **not** run `git commit` unless the user explicitly asks — just propose it.
- Read the diff and describe what actually changed; never guess from filenames.
- No co-author or tool trailers unless the user requests them.
