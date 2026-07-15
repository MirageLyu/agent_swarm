---
name: handoff-project
description: Create a project-local structured session handoff in .claude/handoffs for future sessions to resume from.
---

# Project Handoff

Create a structured session handoff document for continuity across sessions. This is the project-local variant based on Robert Guss' handoff skill.

Use when ending a work session, switching contexts, taking a break mid-task, or before a context reset when the handoff should be kept with this project.

## Output Location

Write the file to:

```text
.claude/handoffs/YYYY-MM-DD-HHMM-<brief-description>.md
```

Create `.claude/handoffs/` if it does not exist.

Use a unique timestamped filename. Do not overwrite an existing handoff.

## Handoff Process

### Step 1: Assess Session State

Capture:

1. What phase the work is in: exploration, planning, implementation, debugging, or review.
2. The active task and user goal.
3. How far along the work is.
4. What changed in code, docs, tests, config, or plans.
5. Which questions, blockers, or risks remain.

### Step 2: Write the Handoff

Use this structure:

```markdown
# Session Handoff: <brief description>

**Date:** <YYYY-MM-DD>

**Project:** <project name/path>

**Branch:** <current branch if known>

**Phase:** <exploration|planning|implementation|debugging|review>

**Focus:** <what the next session should understand>

## Current State

**Task:** <what we are working on>

**Progress:** <where we are: percentage, milestone, or plain status>

## What We Did

<2-5 sentence summary of the session work.>

## Decisions Made

- **<decision>** — <rationale>

## Code Changes

**Files modified:**

- `<path>` — <what changed and why>

**Key code context:** <critical patterns, snippets, or behavior to remember, if any.>

## Open Questions

- [ ] <question needing resolution>

## Blockers / Issues

- <issue> — <current status>

## Context to Remember

<Important background, constraints, user preferences, or domain knowledge that would take time to re-establish.>

## Next Steps

1. [ ] <first concrete step for a future session>
2. [ ] <second concrete step>

## Files to Review on Resume

- `<path>:<line>` — <why it matters>
```

## What to Capture

Always include:

- Decisions with reasoning.
- Code or documentation changes with file paths.
- Current progress.
- Clear next steps.
- User constraints, preferences, and project-specific context.

Include when relevant:

- Errors encountered and whether they were resolved.
- Dead ends or failed approaches.
- Key files to read first.
- External dependencies, APIs, services, or tools involved.

Skip:

- Verbose tool output.
- Repeated similar operations.
- Intermediate reasoning that reached a conclusion.
- Information obvious from the code or already recorded in referenced artifacts.

## Artifact and Secret Rules

Do not duplicate large existing artifacts such as PRDs, plans, ADRs, issues, commits, diffs, mission reports, or test reports. Reference them by path, commit, or URL.

Do not include raw API keys, tokens, passwords, cookies, private keys, or unnecessary personal information. If a secret-bearing setting matters, mention the setting name or file path without copying the value.

## Quality Check

Before reporting done, verify:

1. The file is under `.claude/handoffs/`.
2. The document has all required sections.
3. Decisions include rationale.
4. File references are specific.
5. Next steps are actionable.
6. No obvious secret-like values are present.
