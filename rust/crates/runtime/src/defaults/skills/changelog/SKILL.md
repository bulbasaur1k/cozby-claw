---
name: changelog
description: Maintain a Keep-a-Changelog CHANGELOG.md from commits / the current changes.
---

# changelog

Produce or update a human-readable changelog following the Keep a Changelog format
with Semantic Versioning. Derive entries from real commits/diffs, not guesses.

## Steps

1. Read recent history: `git log <last-tag>..HEAD --oneline` and the diff if needed.
2. Update `CHANGELOG.md` (create if missing) with an `## [Unreleased]` section and
   group entries under these headings (omit empty ones):
   `Added`, `Changed`, `Deprecated`, `Removed`, `Fixed`, `Security`.
3. Write each entry as a short, user-facing sentence — what changed and why it
   matters to a user, not the implementation detail.
4. On release, rename `[Unreleased]` to `## [X.Y.Z] - YYYY-MM-DD` and pick the
   version by SemVer (breaking → major, feature → minor, fix → patch).

## Rules

- Audience is users/integrators, not the committer. No commit hashes in entries.
- One line per change; link issues/PRs if the project does.
- Keep newest version on top.
