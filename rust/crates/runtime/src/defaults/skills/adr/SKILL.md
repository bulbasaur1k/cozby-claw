---
name: adr
description: Write an Architecture Decision Record (Nygard/MADR) — context, decision, consequences.
---

# adr

Capture one architectural decision as a short, reviewable Markdown record in the
repo. A rigid template is what makes the output good — fill it from the real code
and discussion, don't waffle.

## Steps

1. Find the ADR directory (`docs/adr/`, `doc/adr/`, `docs/decisions/`) and the
   next number (`NNNN`). If none exists, propose `docs/adr/0001-...md`.
2. Write `NNNN-short-title.md` using this template:

   ```markdown
   # NNNN. <Short decision title>

   - Status: Proposed        <!-- Proposed | Accepted | Deprecated | Superseded by NNNN -->
   - Date: <YYYY-MM-DD>

   ## Context
   <The forces at play: problem, constraints, what we know. Cite real code/files.>

   ## Decision
   <The choice made, in active voice: "We will …".>

   ## Consequences
   <Positive, negative, and neutral results. What becomes easier/harder. Trade-offs accepted.>

   ## Alternatives considered
   <Each option + why it was rejected.>
   ```

3. Keep it to one page. Ground every claim in the actual system.

## Rules

- One decision per ADR. Don't restate general theory — be specific to this project.
- Never edit an accepted ADR's decision; supersede it with a new one instead.
