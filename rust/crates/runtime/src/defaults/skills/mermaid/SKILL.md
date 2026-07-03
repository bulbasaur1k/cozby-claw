---
name: mermaid
description: Produce valid, reviewable Mermaid diagrams (flowchart, sequence, class, ER, state) in Markdown.
---

# mermaid

Generate diagrams as text-based Mermaid embedded in Markdown — Git-diffable and
rendered natively by GitHub/GitLab. Prefer Mermaid over PlantUML (no render server,
weak models produce valid Mermaid more reliably).

## Steps

1. Pick the diagram type for the intent:
   - **flowchart** (`flowchart TD`) — control/data flow, decisions.
   - **sequenceDiagram** — interactions/messages over time.
   - **classDiagram** — types, fields, relationships.
   - **erDiagram** — database schema.
   - **stateDiagram-v2** — state machines/lifecycles.
2. Base the diagram on the real code (use `grounding` to get actual names).
3. Emit it in a fenced block:

   ````markdown
   ```mermaid
   flowchart TD
     A[Start] --> B{Decision}
     B -->|yes| C[Do thing]
     B -->|no| D[Skip]
   ```
   ````

4. Keep node labels short; split very large diagrams into a few focused ones.

## Rules

- Verify the syntax is valid Mermaid before presenting (balanced brackets, valid arrows).
- Use real component/function names, not placeholders, when diagramming this codebase.
- Only reach for PlantUML when Mermaid genuinely can't express it (e.g. detailed C4).
