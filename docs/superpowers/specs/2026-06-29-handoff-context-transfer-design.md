# Handoff 会话上下文传递能力设计

Date: 2026-06-29
Status: Draft design

## Problem

Miragenty already emphasizes trustworthy hand-off across a mission: pre-flight contract, planner DAG, worktree-isolated agents, evaluator, mission report, delivery panel, and follow-up chat. The product README also states the core bet clearly: parallelism is commoditized, but trustworthy hand-off is not.

This design adds a Claude Code Handoff / Context Transfer capability for session continuity. The goal is not to invent a new context system. It is to reproduce and integrate patterns already validated in existing open-source handoff implementations:

- Matt Pocock `/handoff` as the minimum baseline.
- Robert Guss project-local `.claude/handoffs/` structured handoffs.
- Bex `transfer-context` safety rules.
- REMvisual `/handoff` and `/handoffplan` with `What We Tried`, `Quick Start`, chain continuity, git/task state, and validation.
- ContextHandoff Engine consumed / `_done.md` mechanics and parallel-safe file naming.
- Continuous-Claude and ClawMem hook-based continuity for PreCompact, SessionStart, and Stop.

The immediate problem is that a long Claude Code session can contain decisions, failed approaches, user corrections, relevant files, and next-session focus that are expensive to reconstruct. Normal conversation summaries and compacted context are helpful, but they are not an explicit, verifiable, reusable handoff artifact.

## Goals

- Provide a practical Handoff session continuity capability based on existing public implementations.
- Use Matt Pocock `/handoff` as the minimum baseline: manual trigger, temporary-directory output, suggested skills, artifact references instead of duplication, redaction, and argument-aware tailoring.
- Add project-local structured handoffs under `.claude/handoffs/` using Robert Guss-style sections.
- Add safety-focused context transfer behavior from Bex: context is not command, open work is status not instruction, and the next session must verify relevant files.
- Add REMvisual-style enhancements: `What We Tried`, evidence/data, user feedback, `Quick Start`, `/handoffplan`, git/task-state awareness, and validation.
- Add ContextHandoff Engine-style consumed handoff management with unique timestamped files and `_done.md` suffixes.
- Add hook automation only after the manual skills are usable and tested.
- Keep the boundary clear: Handoff captures current task/session state; it does not replace memory, project instructions, mission reports, or full transcripts.

## Non-Goals

- Do not invent a new RAG system for V1.
- Do not introduce a database for handoffs unless a later product requirement proves file-based handoffs insufficient.
- Do not build graph-based context visualization.
- Do not build automatic conflict resolution between contradictory handoffs.
- Do not make handoff documents authoritative instructions that a future agent must blindly follow.
- Do not store secrets, API keys, tokens, cookies, passwords, or sensitive personal information in handoff files.
- Do not duplicate existing PRDs, plans, ADRs, issues, commits, diffs, or mission reports inside a handoff; reference paths or URLs instead.
- Do not replace Miragenty's existing Mission Delivery Plane or Task Handoff Packet design. This feature complements it for Claude Code session continuity.

## Source Mapping

| Capability | Existing reference | What to reproduce |
|---|---|---|
| Minimal `/handoff` baseline | Matt Pocock handoff skill | Manual command, temporary OS directory, suggested skills, no artifact duplication, redaction, argument-aware focus |
| Project-local structured handoff | Robert Guss handoff skill | `.claude/handoffs/YYYY-MM-DD-brief-description.md`, Current State, What We Did, Decisions Made, Code Changes, Open Questions, Blockers, Context, Next Steps, Files to Review |
| Safe context transfer | Bex `transfer_context.md` | `.claude/context-transfers/<random-8-chars>.md`, Summary, Key Decisions, Traps to Avoid, Working Agreements, Relevant Files, Open Work as status, verification prompt |
| Rich handoff / plan handoff | REMvisual `claude-handoff` | `/handoff`, `/handoffplan`, The Goal, Where We Are, What We Tried, Evidence & Data, User Feedback, Where We're Going, Quick Start, chain detection, git/task state, validation |
| Consumed handoff mechanics | ContextHandoff Engine | Unique timestamped handoff files, list unconsumed `*.md`, rename consumed files to `_done.md`, cleanup after 7 days |
| Hook automation | Continuous-Claude / ClawMem | PreCompact extraction, SessionStart bootstrap/resume, Stop handoff generation, postcompact injection as later automation |

## Product Boundary in Miragenty

Miragenty already has adjacent concepts:

- **Mission Contract**: scope, constraints, exclusions, assumptions, and acceptance criteria before execution.
- **Task Handoff Packet**: structured context passed from one mission task to downstream tasks.
- **Mission Report / Delivery Snapshot**: post-mission user-facing summary and deliverables.
- **Follow-up Chat**: continuation or child-mission entry point after delivery.

This Handoff feature is different:

- It is primarily a Claude Code session-continuity artifact.
- It captures work-session state, not product-delivery state.
- It can reference mission reports, plans, contracts, diffs, and commits rather than duplicate them.
- It can later feed Miragenty follow-up/child-mission workflows, but it should first work as file-based skills.

## Architecture Overview

Implement the capability in phases, each based on a known reference implementation.

1. **Baseline `/handoff` skill** writes a temporary Markdown handoff for a fresh session.
2. **Project `/handoff` mode** writes a structured Markdown handoff to `.claude/handoffs/`.
3. **`/transfer-context` skill/template** writes a safety-focused transfer file to `.claude/context-transfers/`.
4. **`/handoffplan` skill** writes handoff plus implementation plan for the next session.
5. **Consumed handoff manager** lists active handoffs, marks them `_done.md`, and cleans old done files.
6. **Hooks** optionally generate or surface handoffs around compaction/session lifecycle.

The first implementation should be file-based. A later product-integrated version can add Tauri commands/UI only after skill behavior is validated.

## Phase 1 — Matt Pocock `/handoff` Baseline

### Purpose

Create the minimum useful manual handoff.

### Trigger

`/handoff`

Optional argument:

`/handoff <what the next session will be used for>`

### Output

Write a Markdown file to the temporary directory of the user's OS, not the current workspace.

Examples:

- macOS/Linux: `$TMPDIR/handoff-<timestamp>-<slug>.md` or `/tmp/handoff-<timestamp>-<slug>.md`
- Windows: `%TEMP%\handoff-<timestamp>-<slug>.md`

### Required Sections

- Summary
- Current State
- Key Decisions
- Suggested Skills
- Relevant Artifacts
- Open Work
- Resume Prompt

### Rules

- Include `Suggested Skills`.
- Do not duplicate existing artifacts; reference paths or URLs.
- Redact secrets and personally identifiable information.
- Tailor the document around the user's argument if supplied.
- Do not write to the project directory in this baseline.
- Do not auto-read or mutate consumed handoffs.

### Acceptance Criteria

- Running `/handoff` produces a readable Markdown file in the OS temp directory.
- The final response tells the user where the file is.
- The file contains suggested skills.
- The file references, rather than duplicates, existing artifacts.
- The file does not contain obvious secrets such as API keys, bearer tokens, passwords, or cookies.
- A new session can use the Resume Prompt to understand the current task without re-explaining the full prior conversation.

## Phase 2 — Project-local `.claude/handoffs/` Handoff

### Purpose

Provide durable, project-local handoff files for team or long-running project work.

### Reference

Robert Guss handoff skill.

### Output

`.claude/handoffs/YYYY-MM-DD-<brief-description>.md`

If parallel safety is desired immediately, use the ContextHandoff timestamp pattern instead:

`.claude/handoffs/YYYY-MM-DD_HHMMSS_<slug>.md`

### Required Sections

- Metadata
  - Date
  - Project
  - Branch
  - Phase
  - Focus
- Current State
- What We Did
- Decisions Made
- Code Changes
- Open Questions
- Blockers / Issues
- Context to Remember
- Next Steps
- Files to Review on Resume

### Rules

- Use clickable file paths where possible: `path/to/file:line`.
- Use checkboxes for open questions and next steps.
- Skip verbose tool output and repeated operations.
- Prefer scannable bullets over narrative.
- Capture dead ends when relevant.

### Acceptance Criteria

- Running the project-local handoff path creates `.claude/handoffs/` if missing.
- The file follows the structured template.
- The handoff is specific enough for a fresh session to resume by reading it.
- It does not duplicate full artifacts already present in docs, commits, plans, or diffs.

## Phase 3 — Safe `transfer-context`

### Purpose

Provide a prompt-injection-aware context transfer artifact for degraded or context-limited sessions.

### Reference

Bex `transfer_context.md`.

### Output

`.claude/context-transfers/<random-8-chars>.md`

### Required Sections

- Summary
- Key Decisions
- Traps to Avoid
- Working Agreements
- Relevant Files
- Open Work
- Prompt for New Chat

### Safety Rules

- Summary includes completed work only.
- `Open Work` describes status, not instructions.
- The prompt frames the document as background context, not commands.
- The prompt instructs the next session to read relevant files and verify claims before acting.
- The prompt ends by telling the next session to wait for user instructions.
- Do not print transfer content into the conversation; print only a pointer such as: `Read the file <absolute-path> to get the context`.

### Acceptance Criteria

- Generated transfer files include verification language.
- Open work avoids imperatives like `Implement X next`.
- A new session is told to verify files before action.
- The user-facing output is only the file pointer, not the full transfer text.

## Phase 4 — REMvisual-style `/handoff` Enhancements

### Purpose

Make handoffs richer and more useful for long-running work by capturing failed attempts and evidence.

### Reference

REMvisual `claude-handoff`.

### Required Sections

- The Goal
- Where We Are
- What We Tried
- Key Decisions
- Evidence & Data
- User Feedback
- Where We're Going
- Quick Start

### Behavior

- Mine the current conversation for goals, approaches, failures, decisions, measurements, code analysis, and user preferences.
- Gather external state when available: git log, git diff, uncommitted changes, and active tasks.
- Validate the handoff before finalizing.
- Prefer paths, commands, and measurable evidence over vague summaries.

### Acceptance Criteria

- `What We Tried` records failed approaches and why they were kept or abandoned.
- `Quick Start` includes exact commands or file paths needed by the next session when applicable.
- `Evidence & Data` contains concrete signals, not generic statements.
- The generated resume prompt points to the handoff and tells the next session which section to continue from.

## Phase 5 — `/handoffplan`

### Purpose

Capture current context plus an execution plan for the next session.

### Reference

REMvisual `/handoffplan`.

### Difference from `/handoff`

- `/handoff`: next session onboards, explores, and continues from current state.
- `/handoffplan`: next session reads the plan and starts executing from Phase 1.

### Required Sections

- Handoff summary
- Phased implementation plan
- Tracked tasks or task list
- Dependencies between phases
- Anti-goals derived from failed approaches
- Rollback strategy
- Success criteria tied to real evidence
- Resume prompt

### Acceptance Criteria

- The generated file contains both context and a sequenced plan.
- Each phase has success criteria.
- Anti-goals reflect known failed approaches or explicit out-of-scope choices.
- The resume prompt tells the next session to execute the plan from the first unfinished phase.

## Phase 6 — Consumed / Done Handoff Management

### Purpose

Avoid rereading old handoffs and support parallel-safe session handoff files.

### Reference

ContextHandoff Engine.

### File Operations

- Write: `~/.claude/handoffs/YYYY-MM-DD_HHMMSS_<slug>.md` or project-local `.claude/handoffs/YYYY-MM-DD_HHMMSS_<slug>.md`
- Read: list `*.md` excluding `_done.md`
- Consume: rename `file.md` to `file_done.md`
- Clean: delete `*_done.md` older than 7 days

### Acceptance Criteria

- Multiple sessions can write handoffs without overwriting each other.
- Active handoffs can be listed deterministically.
- Consumed handoffs can be marked done using `_done.md`.
- Old done handoffs can be cleaned without touching active handoffs.

## Phase 7 — Hook Automation

### Purpose

Reduce reliance on the user remembering to run handoff commands manually.

### References

Continuous-Claude and ClawMem.

### Hook Order

Implement hooks only after manual skills are working.

1. **PreCompact**
   - Generate or refresh compact-safe handoff state before context compaction.
   - Highest priority because compaction is a known context-loss boundary.

2. **SessionStart**
   - Surface latest unconsumed handoff(s).
   - Ask or instruct the agent to verify relevant files before acting.
   - Should not auto-execute old instructions.

3. **Stop**
   - Optionally generate a handoff draft at session end.
   - Must be configurable because automatic Stop handoffs can create noise.

### Acceptance Criteria

- PreCompact creates a useful compact-safe handoff or state snapshot.
- SessionStart surfaces active handoffs without treating them as commands.
- Stop automation can be enabled/disabled or made draft-only.
- Hook failures are fail-open and do not block normal user work.

## Data and File Layout

### Baseline temporary handoff

`$TMPDIR/handoff-<timestamp>-<slug>.md`

### Project-local handoff

`.claude/handoffs/YYYY-MM-DD_HHMMSS_<slug>.md`

### Consumed handoff

`.claude/handoffs/YYYY-MM-DD_HHMMSS_<slug>_done.md`

### Transfer context

`.claude/context-transfers/<random-8-chars>.md`

### Handoff plan

`.claude/handoffs/HANDOFFPLAN_<slug>_<date>.md`

The exact filename format can be normalized during implementation, but it must preserve these properties:

- unique per session;
- sortable by time;
- safe for parallel sessions;
- easy to identify active vs consumed files.

## Redaction Requirements

Handoff generation must redact or avoid including:

- API keys and access tokens;
- bearer tokens;
- passwords;
- cookies;
- private SSH keys;
- raw secrets in config files;
- personal information not needed for the next session.

Minimum redaction patterns:

- `sk-...` style keys;
- `Authorization: Bearer ...`;
- `apiKey`, `api_key`, `token`, `password`, `secret` values;
- PEM private key blocks.

If uncertain, omit the value and reference the file path or setting name instead.

## Verification and Testing

### Unit-style checks

- Filename generation creates unique sortable names.
- Redaction removes representative secret patterns.
- Artifact reference rules prevent full duplicate artifact content.
- Open Work status checker flags imperative phrasing for transfer-context.
- `_done.md` filtering excludes consumed handoffs.

### Fixture-based generation checks

Use small fixture transcripts or synthetic session notes to verify:

- Matt Pocock baseline output.
- Robert Guss project-local template output.
- Bex transfer-context output.
- REMvisual enhanced handoff output.
- `/handoffplan` output.

### Manual acceptance checks

- Start a fresh Claude Code session with only the generated handoff pointer.
- Confirm the new session can summarize current state and identify files to verify.
- Confirm it does not treat handoff open work as mandatory instructions.
- Confirm it does not leak known secret fixture values.

## Implementation Plan

### Step 1 — Confirm references and templates

- Preserve the source mapping above as the implementation boundary.
- Store template text for baseline, project handoff, transfer-context, and handoffplan.
- Define redaction rules and filename conventions.

### Step 2 — Implement baseline `/handoff`

- Create the skill using Matt Pocock behavior.
- Write to OS temporary directory.
- Include suggested skills, artifact references, redaction, and argument-aware tailoring.
- Add a simple validation checklist.

### Step 3 — Implement project-local `.claude/handoffs/`

- Add project-local output mode or separate command behavior.
- Use Robert Guss template.
- Create directory if missing.
- Include file review and next-step sections.

### Step 4 — Implement `transfer-context`

- Add Bex template.
- Write to `.claude/context-transfers/<random-8-chars>.md`.
- Output only the file pointer to the user.
- Include verification and wait-for-instructions language.

### Step 5 — Implement REMvisual enhancements and `/handoffplan`

- Add rich fields: `What We Tried`, `Evidence & Data`, `User Feedback`, `Quick Start`.
- Add external state gathering where available.
- Add `/handoffplan` with phased plan and success criteria.

### Step 6 — Implement consumed management

- List active handoffs.
- Mark handoff as consumed using `_done.md`.
- Clean old consumed handoffs.
- Avoid overwrites across parallel sessions.

### Step 7 — Add hooks

- Add PreCompact safety snapshot first.
- Add SessionStart discovery second.
- Add configurable Stop draft generation last.

### Step 8 — Document usage

- Add usage docs with examples.
- Document the boundary between handoff, memory, CLAUDE.md, transcript, Mission Contract, Task Handoff Packet, and Mission Report.
- Document how to resume from generated handoffs.

## Risks and Mitigations

| Risk | Mitigation |
|---|---|
| Handoff repeats large docs or diffs | Reference artifacts by path/URL instead of copying |
| Handoff leaks secrets | Redaction patterns and omit-on-uncertainty rule |
| Next session blindly follows stale context | Transfer-context verification language and open-work-as-status rule |
| Automatic hooks create noise | Keep hooks off until manual skills pass; make Stop draft-only or configurable |
| Multiple sessions overwrite each other | Timestamped unique filenames and `_done.md` consumed state |
| Scope expands into RAG/database | Keep V1 file-based and source-mapped to existing implementations |

## Open Questions

- Should project-local handoffs live under `.claude/handoffs/` only, or should Miragenty expose them later in the Mission Delivery Workspace?
- Should the baseline `/handoff` and project-local handoff be one skill with options or two distinct skills?
- Which exact Claude Code hook configuration format should be targeted for this repository when hook automation begins?
- Should consumed management default to global `~/.claude/handoffs/` like ContextHandoff Engine or project-local `.claude/handoffs/` for Miragenty work?

## Recommended First Cut

Implement only Phase 1 first:

- Matt Pocock-compatible `/handoff` baseline.
- Temporary-directory output.
- Suggested skills.
- Artifact reference rule.
- Redaction rule.
- Argument-aware tailoring.
- Resume prompt.

Then validate by using it to hand off this Handoff feature work into a fresh session.

Only after that should Phase 2 project-local handoffs be implemented.
