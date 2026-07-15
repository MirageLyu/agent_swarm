---
name: handoffplan
description: Capture current session context plus a phased implementation plan for the next session to execute.
---

# Handoff Plan

Create a handoff plus execution plan for a fresh session. This skill is based on REMvisual `claude-handoff` and its `/handoffplan` behavior.

Use this when research or design is complete and the next session should execute a plan rather than rediscover context.

## Output Location

Write the file to:

```text
.claude/handoffs/HANDOFFPLAN_<slug>_<YYYY-MM-DD_HHMMSS>.md
```

Create `.claude/handoffs/` if it does not exist. Do not overwrite existing files.

## Difference from Handoff

- `/handoff` captures context so the next session can onboard, explore, and continue from `Where We're Going`.
- `/handoffplan` captures context plus a phased plan so the next session can start executing Phase 1.

## Required Sections

```markdown
# Handoff Plan: <brief title>

## The Goal

<What we are solving and why.>

## Where We Are

- <15-25 bullets if the session was large; fewer if simple.>

## What We Tried

- <Approach tried> — <what happened> — <kept, abandoned, or pending and why.>

## Key Decisions

- **<decision>** — <why chosen; include rejected alternatives when known.>

## Evidence & Data

- <Concrete command, measurement, file path, test result, or observed behavior.>

## User Feedback

- <User preference, correction, tone guidance, or constraint that matters.>

## Where We're Going

<Short summary of the intended next direction.>

## Phased Plan

### Phase 1: <name>

- Goal:
- Files:
- Steps:
  - [ ] <step>
- Success Criteria:
- Rollback / Recovery:

### Phase 2: <name>

- Goal:
- Files:
- Steps:
  - [ ] <step>
- Success Criteria:
- Rollback / Recovery:

## Anti-Goals

- <What not to do, based on failed approaches, explicit scope, or user instruction.>

## Quick Start

```bash
<exact command if applicable>
```

## Resume Prompt

Read `<this handoff plan path>` and execute the plan starting at the first unfinished phase. Verify referenced files before modifying anything.
```

## Requirements

- Include `What We Tried`; failed approaches are expensive context and should not be rediscovered.
- Include `Quick Start` with exact commands, paths, or first files to read when known.
- Tie success criteria to concrete evidence: tests, validation scripts, file existence, or user-visible behavior.
- Include anti-goals derived from failed approaches and explicit out-of-scope decisions.
- Reference existing artifacts by path or URL rather than copying them.
- Redact secrets and unnecessary personal information.

## Quality Check

Before reporting done, verify:

1. The file contains both context and a phased plan.
2. Every phase has success criteria.
3. `What We Tried` records failed or abandoned approaches when any exist.
4. `Quick Start` is actionable.
5. The resume prompt names the handoff plan path.
6. No obvious secret-like values are present.
