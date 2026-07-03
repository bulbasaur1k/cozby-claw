---
name: pr-description
description: Draft a pull-request title and description from the branch diff.
---

# pr-description

Draft a clear pull-request title and body from the changes on the current branch.

## Steps

1. Determine the base branch (usually `main`) and read the branch diff:
   `git log <base>..HEAD --oneline` and `git diff <base>...HEAD`.
2. Write a **title**: `type(scope): summary`, imperative, ≤ 72 chars.
3. Write a **body** with these sections (drop any that do not apply):
   - **Summary** — what this PR does and why, in 1–3 sentences.
   - **Changes** — bullet list of the notable changes.
   - **Testing** — how it was verified (commands run, cases covered).
   - **Notes** — follow-ups, risks, or things reviewers should focus on.
4. Output the title and body as ready-to-paste markdown.

## Rules

- Base the description on the actual diff and commits, not assumptions.
- Keep it tight — reviewers skim; lead with the summary.
