---
name: system-design
description: |
  Architecture and module-decomposition heuristics: pick the smallest viable
  module boundary, document interfaces before implementation, write a short ADR
  for any choice with non-trivial trade-offs.
compatible_roles: [architect, researcher]
---

# System design playbook

## Default checklist before writing a design doc

1. **Restate the problem in one sentence**. If you cannot, the requirement is
   not yet ready for design.
2. **Identify the consumer of the artifact**: is it another agent, a human
   reviewer, or a downstream service? Calibrate detail accordingly.
3. **List the smallest module set**: prefer 1 module over 2 unless there is a
   real coupling reason (different lifecycle, different language, different
   trust boundary).
4. **Enumerate failure modes**: for each module/API, write the top-2 ways it
   can fail and how the caller will know.
5. **Decide what NOT to do**: every design has an out-of-scope section; if
   yours does not, you have not finished the design.

## Default ADR template (300 words max)

```
# ADR-NNN: <decision title>

Status: proposed | accepted | superseded
Context: 2-4 sentences on what forced this decision.
Decision: 1-3 sentences. Be specific.
Consequences:
  - positive: ...
  - negative: ...
  - neutral: ...
Alternatives considered:
  - <option>: rejected because <one sentence>
```

## Anti-patterns

- "Cathedral" diagrams that show every layer: pick the 3-5 boxes that matter.
- Inventing new abstractions when the existing codebase already has a working
  pattern. Reuse first; add new patterns only when the existing one is
  demonstrably insufficient.
- Designing for a future user that has not been requested. YAGNI applies to
  designs as much as to code.
