# Mission Delivery Plane Design

Date: 2026-06-12
Status: Approved design

## Problem

Miragenty currently has a strong execution plane for planning, running DAG tasks, merging worktrees, publishing artifacts, and generating reports. The completed-state product experience is weaker:

1. Downstream DAG nodes receive file state and some artifact metadata, but not enough structured upstream context. Agents can waste time rediscovering decisions, commands, caveats, and reusable findings.
2. After a mission finishes, users may not know what was actually delivered, where it is, how to run it, or whether it was validated. This is especially visible for app-building missions, but the problem is broader than `.app`, `.dmg`, or `.pkg` files.
3. The delivery panel and follow-up chat exist as partial pieces, but completed missions do not consistently transition into a durable delivery workspace for acceptance, explanation, and iteration.

## Goals

- Add a durable Mission Delivery Plane above the existing scheduler/worktree/artifact/report systems.
- Persist structured task handoff packets so downstream agents and users can reuse upstream context.
- Generate a model-curated mission delivery snapshot when a mission completes or fails.
- Treat file scanning as broad candidate discovery, not as hard-coded deliverable rules.
- Make completed/failed missions open into a Delivery Workspace with overview, primary deliverables, usage steps, validation, changes, task handoffs, report, and follow-up chat.
- Keep degraded fallback states usable when model curation, candidate collection, or report generation fails.

## Non-Goals

- Cloud publishing, GitHub Release upload, notarization, signing, or full installer build automation.
- A universal rule-based deliverable classifier.
- Full transcript injection into downstream prompts.
- Large-scale embedding/RAG over all generated files.
- Replacing the existing scheduler, worktree, artifact, report, or chat systems.

## Architecture Overview

Add three connected capabilities:

1. **Task Handoff Packet** — a structured, persisted summary for each completed task.
2. **Delivery Curator** — a post-mission model step that decides what the user should receive and how they should use it.
3. **Delivery Workspace** — the completed-state UI backed by durable delivery snapshots.

Existing systems stay in place:

- `publish_artifact` remains the explicit agent artifact path.
- Worktree merges remain the file-state transfer mechanism.
- Mission reports remain available, but should include delivery data and be auto-generated/refreshed.
- `mission-delivered` remains useful as a realtime event, but the UI should reload from persisted delivery data.
- Follow-up chat remains the iteration mechanism, now seeded with delivery snapshot and handoff context.

## Task Handoff Packets

Each task completion creates or updates one handoff packet. The packet is not a transcript; it is the compact context downstream agents need.

Recommended shape:

```ts
type TaskHandoffPacket = {
  taskId: string
  missionId: string
  title: string
  objective: string
  summary: string
  changedFiles: Array<{
    path: string
    role: string
    changeSummary: string
  }>
  decisions: Array<{
    decision: string
    rationale: string
    alternativesConsidered?: string[]
  }>
  commandsRun: Array<{
    command: string
    purpose: string
    result: "passed" | "failed" | "skipped" | "unknown"
    evidence?: string
  }>
  artifacts: Array<{
    artifactId?: string
    path?: string
    label: string
    purpose: string
    howToUse?: string
  }>
  reusableContext: string[]
  caveats: string[]
  downstreamHints: string[]
  confidence: "high" | "medium" | "low"
}
```

Generation should use a dual path:

- **Agent-authored fields:** extend `task_complete` so the agent can state decisions, changed files, validation, caveats, and downstream hints while the work is fresh.
- **System enrichment/fallback:** after task completion, supplement or repair the packet from git changed files, published artifacts, completion summary, and recent task events. If richer summarization fails, persist a minimal fallback packet rather than blocking the mission.

Downstream prompts should include a clear `Upstream Handoff Packets` section:

- Direct parent tasks: detailed packet content.
- Transitive ancestors: compressed digest.
- Large artifacts: summary plus path/reference, not full content.

Priority under prompt budget pressure:

1. direct parent summary
2. decisions
3. changed files
4. caveats
5. artifacts and usage hints
6. transitive digest

## Delivery Candidate Collection

Candidate collection must be broad and permissive. It should not decide final deliverables.

Candidate sources:

- published artifacts
- task handoff declared deliverables
- newly added or modified files
- executable files
- archives, bundles, installers, packages
- common build output directories
- documents, reports, notebooks, config files, datasets, model checkpoints, demo assets
- package manifests, scripts, README run instructions
- mission goal hints

Candidate shape:

```ts
type DeliveryCandidate = {
  id: string
  path?: string
  uri?: string
  label: string
  candidateKind: string
  source: "artifact" | "handoff" | "git" | "filesystem" | "manifest" | "model_hint"
  evidence: string[]
  sizeBytes?: number
  modifiedAt?: string
}
```

Scanner heuristics may detect `.app`, `.dmg`, `.pkg`, `pkg/**`, `dist/**`, `target/release/bundle/**`, or `src-tauri/target/release/bundle/**`, but these are only candidate hints. They must not become the product definition of delivery.

## Delivery Curator

When a mission reaches completed or failed state, run a Delivery Curator step. The curator receives:

- mission goal and preflight contract
- planner DAG and task titles
- task handoff packets
- published artifacts
- changed files summary
- delivery candidates
- build/test/run validation evidence
- final merge branch/commit information
- existing report data when available

The model decides:

- what the user should primarily receive
- which candidates are supporting material
- how to run, open, install, deploy, or inspect the result
- what validation exists
- what caveats or warnings matter
- what the next reasonable follow-up actions are

Snapshot shape:

```ts
type MissionDeliverySnapshot = {
  missionId: string
  generatedAt: string
  status: "completed" | "completed_with_warnings" | "failed"
  overview: {
    title: string
    summary: string
    userGoal: string
    result: string
  }
  primaryDeliverables: DeliveryItem[]
  supportingDeliverables: DeliveryItem[]
  howToUse: Array<{
    title: string
    steps: string[]
    commands?: string[]
    relatedDeliverableIds?: string[]
  }>
  validation: Array<{
    label: string
    command?: string
    result: "passed" | "failed" | "not_run" | "unknown"
    evidence: string
  }>
  changes: Array<{
    label: string
    summary: string
    files?: string[]
  }>
  caveats: string[]
  nextSteps: string[]
  reportId?: string
}

type DeliveryItem = {
  id: string
  kind: string
  label: string
  path?: string
  uri?: string
  isPrimary: boolean
  whyThisMatters: string
  howToUse?: string
  evidence: string[]
  confidence: "high" | "medium" | "low"
  warnings?: string[]
}
```

`kind` is intentionally open-ended. Examples include `macos_app`, `installer`, `source_project`, `report`, `dataset`, `model_checkpoint`, `deployment_bundle`, `notebook`, `demo`, `configuration`, or `unknown`.

If a mission asked for a macOS app and no app/package is found, the snapshot should not be empty. It should explicitly say what was delivered instead, such as source code and build commands, and warn that a packaged app was not identified.

## Persistence

Add a durable delivery store, preferably a new `mission_deliveries` table:

- `mission_id`
- `version`
- `snapshot_json`
- `created_at`
- `updated_at`
- `curator_model`
- source task/event references where practical

Add a task handoff store, preferably a new table keyed by task id:

- `task_id`
- `mission_id`
- `packet_json`
- `created_at`
- `updated_at`
- generation source/status metadata

Use JSON for the packet/snapshot payloads at first so the schema can evolve without large migrations. Add typed Rust structs at the boundary for validation and frontend serialization.

## Delivery Workspace UI

Completed, completed-with-warnings, and failed missions should default to the Delivery Workspace rather than the DAG view. DAG/events remain available as secondary tabs.

Workspace sections:

1. **Overview** — goal, result, summary, warnings.
2. **Primary Delivery** — model-selected primary deliverables with path/URI, confidence, why it matters, and how to use it.
3. **How to use** — copyable commands and step-by-step instructions.
4. **Validation** — build/test/run evidence with passed/failed/not-run/unknown status.
5. **Supporting deliverables** — secondary files, docs, reports, configs, datasets, or source paths.
6. **What changed** — concise change summary and key files.
7. **Task handoff timeline** — human-readable summary of each agent’s contribution.
8. **Follow-up chat** — always visible as the main iteration path.
9. **Report** — link or embedded view of the mission report.

Degraded UI states:

- snapshot generating: show “Preparing delivery summary…”
- curator failed: show fallback snapshot and retry action
- no obvious package: show source/project path, likely commands, artifacts, and an “ask chat to package/export” CTA
- failed mission: show partial delivery, failure reason, recovery steps, and chat

Actions can include copy path, reveal/open file where supported, copy command, open report, regenerate snapshot/report, and start/confirm follow-up mission.

## Follow-Up Chat Integration

The completed mission chat should receive delivery context:

- delivery snapshot
- primary/supporting deliverables
- how-to-use steps
- validation and caveats
- task handoff summaries

Small follow-up requests can use the existing direct edit/commit path. Larger requests should produce a child mission proposal. The Delivery Workspace should make this flow prominent and easy to continue.

If a follow-up modifies files or a child mission completes, mark the existing snapshot stale and offer regeneration. Child missions should get their own snapshots while preserving parent/child navigation.

## Report Integration

Mission reports should be generated or refreshed automatically after delivery curation when possible. Report rendering should include:

- deliverables
- how-to-use steps
- validation evidence
- caveats
- task handoff summary

The delivery snapshot stores a report reference when available. Report generation failure should not block delivery snapshot persistence.

## Error Handling

- **Handoff generation fails:** persist a fallback packet from task title, completion summary, artifacts, and changed files if available.
- **Candidate collection finds little:** let the curator identify source/project/report/commands as delivery; do not treat absence of app/package as generic failure.
- **Curator output invalid:** validate with schema, retry once, then persist degraded snapshot.
- **Curator call fails:** persist degraded snapshot from deterministic data.
- **Report generation fails:** snapshot remains valid and records a warning or missing report reference.
- **UI reloads after event is lost:** fetch persisted snapshot by mission id.
- **Snapshot stale after follow-up edits:** mark stale and allow regeneration.

## Testing Strategy

Backend unit tests:

- fallback handoff generation
- handoff persistence read/write
- direct-parent handoff loading
- transitive digest selection under budget
- delivery candidate collection from artifacts, changed files, executable/archive/bundle-like paths, and empty inputs
- delivery snapshot validation and degraded fallback
- DB migrations

Backend integration/command tests:

- completed mission generates delivery snapshot
- failed mission generates partial delivery snapshot
- chat context includes delivery snapshot and handoff summaries
- report generation includes deliverables, how-to-use, validation, and caveats

Frontend tests:

- completed mission defaults to Delivery Workspace
- loading, degraded, failed, and missing-snapshot states
- primary delivery cards
- no-package warning for app-like mission without package
- copyable how-to-use commands
- validation section status rendering
- follow-up chat visibility
- ReportView delivery sections

Manual verification:

- run a macOS app mission or fixture that produces `.app`/`.dmg` and confirm Primary Delivery is correct
- run or simulate a mission without package output and confirm source/commands/next steps are shown
- restart the app and confirm the delivery snapshot persists
- ask follow-up chat how to run or package the result and verify it uses delivery context

## Rollout Plan

Implement in phases but ship as one coherent feature branch:

1. DB schema and Rust models for handoff packets and delivery snapshots.
2. Handoff generation, persistence, and downstream prompt injection.
3. Candidate collector and deterministic degraded snapshot builder.
4. LLM Delivery Curator with schema validation and fallback.
5. Automatic report generation/refresh and report rendering additions.
6. Delivery Workspace frontend and chat context integration.
7. Tests and manual verification.

The first implementation should optimize for durable correctness and clear UX over perfect deliverable classification. The curator prompt and schema can evolve once real missions produce more examples.
