---
name: handoff
description: Compact the current conversation into a handoff document for another agent to pick up.
---

# Handoff

Write a handoff document summarising the current conversation so a fresh agent can continue the work. Save it to the temporary directory of the user's OS, not the current workspace.

If the user passed arguments, treat them as a description of what the next session will focus on and tailor the document accordingly.

## Output Location

Save the handoff file outside the repository:

- macOS / Linux: use `$TMPDIR` when available, otherwise `/tmp`.
- Windows: use `%TEMP%`.

Use a filename like:

```text
handoff-YYYYMMDD-HHMMSS-<short-slug>.md
```

After writing the file, tell the user the absolute path.

## Required Sections

The handoff document must include these sections:

```markdown
# Handoff: <short title>

## Summary

<What this session was about and what was accomplished.>

## Current State

<Where the work stands now. Include what is complete, incomplete, blocked, or uncertain.>

## Key Decisions

- <Decision> — <reasoning>

## Suggested Skills

- `<skill-name>` — <why the next session may need it>

## Relevant Artifacts

- `<path-or-url>` — <why it matters>

## Open Work

- <Remaining work or unresolved status.>

## Resume Prompt

<Paste-ready prompt for a fresh session.>
```

## Rules

### Include Suggested Skills

Include a `Suggested Skills` section. Recommend only skills that are relevant to the next session's focus. If no specific skill is known, say that no specific skill is required rather than inventing one.

### Reference Existing Artifacts Instead of Duplicating Them

Do not duplicate content already captured in other artifacts, including:

- PRDs
- plans
- ADRs
- issues
- commits
- diffs
- design docs
- test reports
- mission reports

Reference them by path, commit, or URL instead.

### Redact Sensitive Information

Do not include raw sensitive information such as:

- API keys
- access tokens
- bearer tokens
- passwords
- cookies
- private keys
- personally identifiable information that is not needed for continuity

If a detail matters, refer to the setting or file path without copying the secret value.

### Tailor to Arguments

If the user supplied arguments to `/handoff`, treat them as the intended purpose of the next session. Prioritize context, artifacts, decisions, and open work relevant to that purpose.

### Keep It Concise

The document should be compact and useful. Prefer specific bullets over long narrative. Avoid verbose tool output and repeated operation logs.

## Quality Check Before Reporting Done

Before reporting the handoff path, verify the document:

1. Is saved outside the current workspace.
2. Has all required sections.
3. Contains a `Suggested Skills` section.
4. References existing artifacts instead of copying them.
5. Does not contain obvious secrets or token-like values.
6. Includes a paste-ready `Resume Prompt`.
