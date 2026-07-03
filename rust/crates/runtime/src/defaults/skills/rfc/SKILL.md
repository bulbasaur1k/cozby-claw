---
name: rfc
description: Draft an RFC / design proposal from a template — problem, proposal, alternatives, rollout.
---

# rfc

Write a design proposal (RFC) for a non-trivial change so it can be reviewed
before implementation. Constrain hard with the template and cite real code — RFCs
are prose-heavy, which is where a weak model waffles.

## Steps

1. Create `docs/rfcs/NNNN-short-title.md` (or the repo's existing location/format).
2. Use this template:

   ```markdown
   # RFC NNNN: <Title>

   - Status: Draft      <!-- Draft | In review | Accepted | Rejected -->
   - Author: <name>
   - Date: <YYYY-MM-DD>

   ## Summary
   <One paragraph: what and why.>

   ## Motivation
   <The problem. Who is affected. Why now. Reference real files/limitations.>

   ## Proposal
   <The design in detail: components, data flow, API/interface changes, migration.>

   ## Alternatives
   <Other options and why they were not chosen.>

   ## Drawbacks & risks
   <Costs, risks, what could go wrong.>

   ## Rollout & testing
   <How it ships, is tested, and is rolled back.>

   ## Open questions
   <Unresolved points for reviewers.>
   ```

3. Keep each section tight; link to code rather than pasting large blocks.

## Rules

- Every claim about the current system must cite a real file/function.
- Prefer a smaller proposal that ships over a grand one that doesn't.
