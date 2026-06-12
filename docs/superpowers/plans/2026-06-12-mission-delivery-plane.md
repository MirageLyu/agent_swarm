# Mission Delivery Plane Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a durable Mission Delivery Plane that persists task handoff packets, model-curated delivery snapshots, completed-mission delivery UI, report integration, and follow-up chat context.

**Architecture:** Add a backend delivery domain module with JSON-backed DB persistence for task handoffs and mission delivery snapshots. Generate handoffs at task completion, inject upstream handoffs into coding-agent prompts, generate a delivery snapshot when missions complete/fail or on demand, and render the snapshot in a new Delivery Workspace embedded in the Missions view.

**Tech Stack:** Rust/Tauri v2 backend, rusqlite migrations and query helpers, serde JSON models, existing LLM provider abstraction, React + TypeScript frontend, CSS modules, Vitest/React Testing Library, Cargo tests.

---

## Scope Check

The approved spec spans several surfaces, but they form one vertical product feature: data persistence -> agent context transfer -> delivery curation -> completed-state UI -> chat/report integration. Implement this as one feature branch with small commits after each task. Do not split into separate specs unless a task reveals a blocking architectural constraint.

## File Structure

### Backend files to create

- `src-tauri/src/agent/delivery.rs`
  - Owns serializable delivery-domain structs (`TaskHandoffPacket`, `MissionDeliverySnapshot`, `DeliveryCandidate`, `DeliveryItem`).
  - Builds fallback handoff packets.
  - Collects broad delivery candidates.
  - Builds deterministic degraded delivery snapshots.
  - Runs the optional LLM curator when a provider is available.
  - Renders compact delivery/handoff prompt blocks for coding agents and chat.

- `src-tauri/src/commands/delivery.rs`
  - Tauri IPC commands for `get_mission_delivery` and `generate_mission_delivery`.
  - Thin wrapper around `agent::delivery` and `db::queries`.

### Backend files to modify

- `src-tauri/src/db/migrations.rs`
  - Add a migration for `task_handoff_packets` and `mission_deliveries`.
  - Add migration tests.

- `src-tauri/src/db/queries.rs`
  - Add row structs and helpers for task handoffs and mission delivery snapshots.

- `src-tauri/src/agent/mod.rs`
  - Export the new `delivery` module.

- `src-tauri/src/commands/mod.rs`
  - Export the new `delivery` command module.

- `src-tauri/src/lib.rs`
  - Register new IPC commands.

- `src-tauri/src/tools/definitions.rs`
  - Extend `task_complete` input schema with optional handoff fields.

- `src-tauri/src/agent/engine.rs`
  - Parse optional handoff fields from `task_complete`.
  - Persist a task handoff packet after completion summary persistence.

- `src-tauri/src/agent/codebase_intel.rs`
  - Include upstream handoff packets in the upstream context block.

- `src-tauri/src/agent/scheduler.rs`
  - Trigger best-effort delivery snapshot generation when a mission reaches terminal completed/failed state.

- `src-tauri/src/agent/chat.rs`
  - Add delivery snapshot and handoff context to completed-mission chat prompts.

- `src-tauri/src/commands/chat.rs`
  - Include delivery snapshot summary in follow-up mission proposal context.

- `src-tauri/src/agent/report_generator.rs`
  - Add delivery fields to `MissionReport` and Markdown output.

### Frontend files to create

- `src/components/mission/DeliveryWorkspace.tsx`
  - Completed/failed mission workspace container.
  - Loads persistent delivery snapshot.
  - Renders overview, primary delivery, how-to-use, validation, supporting deliverables, changes, handoff timeline, report action, and chat.

- `src/components/mission/DeliveryWorkspace.module.css`
  - Component-local layout/styles.

- `src/components/mission/DeliveryWorkspace.test.tsx`
  - Component tests for loading, degraded, no-package warning, primary delivery, and chat visibility.

- `src/components/report/ReportDeliverySection.tsx`
  - Renders delivery fields inside `ReportView`.

### Frontend files to modify

- `src/ipc/commands.ts`
  - Add delivery TypeScript types and command wrappers.
  - Add optional delivery fields to report types.

- `src/views/MissionsView.tsx`
  - Replace completed-state event-only delivery/chat blocks with `DeliveryWorkspace`.
  - Continue passing live `mission-delivered` payload as a realtime hint only.

- `src/views/MissionsView.module.css`
  - Remove or reduce completed-state layout rules only if superseded by `DeliveryWorkspace.module.css`.

- `src/views/ReportView.tsx`
  - Add delivery section to section list and render order.

- `src/components/report/index.ts`
  - Export `ReportDeliverySection`.

- `src/i18n/locales/en-US.json`
  - Add English labels for Delivery Workspace.

- `src/i18n/locales/zh-CN.json`
  - Add Chinese labels for Delivery Workspace.

---

## Task 1: Add DB persistence for task handoffs and mission deliveries

**Files:**
- Modify: `src-tauri/src/db/migrations.rs`
- Modify: `src-tauri/src/db/queries.rs`

- [ ] **Step 1: Add migration test first**

Add a test module at the bottom of `src-tauri/src/db/migrations.rs`. Use the next migration name from the current `MIGRATIONS` array. If the latest migration is not `041_*`, use the next numeric prefix actually present in the file.

```rust
#[cfg(test)]
mod mission_delivery_plane_migration_tests {
    use super::run;
    use rusqlite::Connection;

    #[test]
    fn migration_creates_handoff_and_delivery_tables() {
        let conn = Connection::open_in_memory().expect("open in-memory db");
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        run(&conn).expect("migrations run");

        let handoff_sql: String = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'task_handoff_packets'",
                [],
                |row| row.get(0),
            )
            .expect("task_handoff_packets table exists");
        assert!(handoff_sql.contains("packet_json"));
        assert!(handoff_sql.contains("generation_status"));

        let delivery_sql: String = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'mission_deliveries'",
                [],
                |row| row.get(0),
            )
            .expect("mission_deliveries table exists");
        assert!(delivery_sql.contains("snapshot_json"));
        assert!(delivery_sql.contains("curator_model"));

        let handoff_indexes: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = 'idx_task_handoffs_mission'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(handoff_indexes, 1);
    }
}
```

- [ ] **Step 2: Run migration test to verify it fails**

Run:

```bash
cd src-tauri
cargo test migration_creates_handoff_and_delivery_tables --lib
```

Expected: FAIL because the tables do not exist.

- [ ] **Step 3: Add the migration**

Append this migration tuple to `MIGRATIONS` in `src-tauri/src/db/migrations.rs`. Adjust the numeric prefix to be one greater than the current latest migration.

```rust
(
    "042_mission_delivery_plane",
    r#"
    CREATE TABLE IF NOT EXISTS task_handoff_packets (
        task_id TEXT PRIMARY KEY REFERENCES tasks(id) ON DELETE CASCADE,
        mission_id TEXT NOT NULL REFERENCES missions(id) ON DELETE CASCADE,
        packet_json TEXT NOT NULL,
        generation_status TEXT NOT NULL DEFAULT 'generated'
            CHECK (generation_status IN ('agent_authored', 'generated', 'fallback')),
        created_at TEXT NOT NULL DEFAULT (datetime('now')),
        updated_at TEXT NOT NULL DEFAULT (datetime('now'))
    );

    CREATE INDEX IF NOT EXISTS idx_task_handoffs_mission
        ON task_handoff_packets(mission_id, updated_at);

    CREATE TABLE IF NOT EXISTS mission_deliveries (
        mission_id TEXT PRIMARY KEY REFERENCES missions(id) ON DELETE CASCADE,
        version INTEGER NOT NULL DEFAULT 1,
        snapshot_json TEXT NOT NULL,
        generation_status TEXT NOT NULL DEFAULT 'generated'
            CHECK (generation_status IN ('generated', 'degraded', 'failed')),
        curator_model TEXT,
        source_task_ids TEXT NOT NULL DEFAULT '[]',
        source_event_ids TEXT NOT NULL DEFAULT '[]',
        stale INTEGER NOT NULL DEFAULT 0,
        created_at TEXT NOT NULL DEFAULT (datetime('now')),
        updated_at TEXT NOT NULL DEFAULT (datetime('now'))
    );

    CREATE INDEX IF NOT EXISTS idx_mission_deliveries_status
        ON mission_deliveries(generation_status, updated_at);
    "#,
),
```

- [ ] **Step 4: Run migration test to verify it passes**

Run:

```bash
cd src-tauri
cargo test migration_creates_handoff_and_delivery_tables --lib
```

Expected: PASS.

- [ ] **Step 5: Add query tests first**

Append tests in `src-tauri/src/db/queries.rs` near existing query tests or at the bottom if there is no delivery-related section.

```rust
#[cfg(test)]
mod mission_delivery_queries_tests {
    use super::*;
    use crate::db::migrations_run_on;
    use rusqlite::Connection;

    fn setup() -> Connection {
        let conn = Connection::open_in_memory().expect("open in-memory db");
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        migrations_run_on(&conn).expect("run migrations");
        conn.execute(
            "INSERT INTO missions (id, title, description, status) VALUES ('m1', 'Mission', 'Build app', 'completed')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tasks (id, mission_id, title, description, status) VALUES ('t1', 'm1', 'Task', 'Do work', 'completed')",
            [],
        )
        .unwrap();
        conn
    }

    #[test]
    fn upserts_and_reads_task_handoff_packet() {
        let conn = setup();
        upsert_task_handoff_packet(&conn, "t1", "m1", "{\"summary\":\"first\"}", "generated")
            .unwrap();
        upsert_task_handoff_packet(&conn, "t1", "m1", "{\"summary\":\"second\"}", "agent_authored")
            .unwrap();

        let row = get_task_handoff_packet(&conn, "t1").unwrap().expect("handoff exists");
        assert_eq!(row.task_id, "t1");
        assert_eq!(row.mission_id, "m1");
        assert_eq!(row.packet_json, "{\"summary\":\"second\"}");
        assert_eq!(row.generation_status, "agent_authored");
    }

    #[test]
    fn lists_parent_handoff_packets_for_task() {
        let conn = setup();
        conn.execute(
            "INSERT INTO tasks (id, mission_id, title, description, status) VALUES ('t2', 'm1', 'Child', 'Continue', 'pending')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO task_dependencies (task_id, depends_on) VALUES ('t2', 't1')",
            [],
        )
        .unwrap();
        upsert_task_handoff_packet(&conn, "t1", "m1", "{\"summary\":\"parent\"}", "generated")
            .unwrap();

        let rows = list_parent_handoff_packets(&conn, "t2").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].task_id, "t1");
        assert!(rows[0].packet_json.contains("parent"));
    }

    #[test]
    fn upserts_and_reads_mission_delivery() {
        let conn = setup();
        upsert_mission_delivery(
            &conn,
            "m1",
            1,
            "{\"overview\":{\"title\":\"First\"}}",
            "degraded",
            Some("fallback"),
            "[\"t1\"]",
            "[]",
            false,
        )
        .unwrap();

        let row = get_mission_delivery(&conn, "m1").unwrap().expect("delivery exists");
        assert_eq!(row.mission_id, "m1");
        assert_eq!(row.version, 1);
        assert_eq!(row.generation_status, "degraded");
        assert_eq!(row.curator_model.as_deref(), Some("fallback"));
        assert!(!row.stale);
    }
}
```

- [ ] **Step 6: Run query tests to verify they fail**

Run:

```bash
cd src-tauri
cargo test mission_delivery_queries_tests --lib
```

Expected: FAIL because query helpers and row structs are missing.

- [ ] **Step 7: Add query row structs and helpers**

Add this section to `src-tauri/src/db/queries.rs` near report/artifact helpers.

```rust
// ---- Mission Delivery Plane helpers ----

#[derive(Debug, Clone, serde::Serialize)]
pub struct TaskHandoffPacketRow {
    pub task_id: String,
    pub mission_id: String,
    pub packet_json: String,
    pub generation_status: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct MissionDeliveryRow {
    pub mission_id: String,
    pub version: i64,
    pub snapshot_json: String,
    pub generation_status: String,
    pub curator_model: Option<String>,
    pub source_task_ids: String,
    pub source_event_ids: String,
    pub stale: bool,
    pub created_at: String,
    pub updated_at: String,
}

fn map_task_handoff_packet_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TaskHandoffPacketRow> {
    Ok(TaskHandoffPacketRow {
        task_id: row.get(0)?,
        mission_id: row.get(1)?,
        packet_json: row.get(2)?,
        generation_status: row.get(3)?,
        created_at: row.get(4)?,
        updated_at: row.get(5)?,
    })
}

fn map_mission_delivery_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<MissionDeliveryRow> {
    let stale_int: i64 = row.get(7)?;
    Ok(MissionDeliveryRow {
        mission_id: row.get(0)?,
        version: row.get(1)?,
        snapshot_json: row.get(2)?,
        generation_status: row.get(3)?,
        curator_model: row.get(4)?,
        source_task_ids: row.get(5)?,
        source_event_ids: row.get(6)?,
        stale: stale_int != 0,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
    })
}

pub fn upsert_task_handoff_packet(
    conn: &Connection,
    task_id: &str,
    mission_id: &str,
    packet_json: &str,
    generation_status: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO task_handoff_packets (task_id, mission_id, packet_json, generation_status, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, datetime('now'), datetime('now')) \
         ON CONFLICT(task_id) DO UPDATE SET \
           packet_json = excluded.packet_json, \
           generation_status = excluded.generation_status, \
           updated_at = datetime('now')",
        params![task_id, mission_id, packet_json, generation_status],
    )?;
    Ok(())
}

pub fn get_task_handoff_packet(
    conn: &Connection,
    task_id: &str,
) -> Result<Option<TaskHandoffPacketRow>> {
    conn.query_row(
        "SELECT task_id, mission_id, packet_json, generation_status, created_at, updated_at \
         FROM task_handoff_packets WHERE task_id = ?1",
        params![task_id],
        map_task_handoff_packet_row,
    )
    .optional()
    .map_err(Into::into)
}

pub fn list_parent_handoff_packets(
    conn: &Connection,
    task_id: &str,
) -> Result<Vec<TaskHandoffPacketRow>> {
    let mut stmt = conn.prepare(
        "SELECT hp.task_id, hp.mission_id, hp.packet_json, hp.generation_status, hp.created_at, hp.updated_at \
         FROM task_dependencies td \
         JOIN task_handoff_packets hp ON hp.task_id = td.depends_on \
         WHERE td.task_id = ?1 \
         ORDER BY hp.updated_at ASC",
    )?;
    let rows = stmt
        .query_map(params![task_id], map_task_handoff_packet_row)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn list_task_handoff_packets_for_mission(
    conn: &Connection,
    mission_id: &str,
) -> Result<Vec<TaskHandoffPacketRow>> {
    let mut stmt = conn.prepare(
        "SELECT task_id, mission_id, packet_json, generation_status, created_at, updated_at \
         FROM task_handoff_packets WHERE mission_id = ?1 ORDER BY updated_at ASC",
    )?;
    let rows = stmt
        .query_map(params![mission_id], map_task_handoff_packet_row)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn upsert_mission_delivery(
    conn: &Connection,
    mission_id: &str,
    version: i64,
    snapshot_json: &str,
    generation_status: &str,
    curator_model: Option<&str>,
    source_task_ids: &str,
    source_event_ids: &str,
    stale: bool,
) -> Result<()> {
    let stale_int = if stale { 1 } else { 0 };
    conn.execute(
        "INSERT INTO mission_deliveries \
         (mission_id, version, snapshot_json, generation_status, curator_model, source_task_ids, source_event_ids, stale, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, datetime('now'), datetime('now')) \
         ON CONFLICT(mission_id) DO UPDATE SET \
           version = excluded.version, \
           snapshot_json = excluded.snapshot_json, \
           generation_status = excluded.generation_status, \
           curator_model = excluded.curator_model, \
           source_task_ids = excluded.source_task_ids, \
           source_event_ids = excluded.source_event_ids, \
           stale = excluded.stale, \
           updated_at = datetime('now')",
        params![
            mission_id,
            version,
            snapshot_json,
            generation_status,
            curator_model,
            source_task_ids,
            source_event_ids,
            stale_int
        ],
    )?;
    Ok(())
}

pub fn get_mission_delivery(
    conn: &Connection,
    mission_id: &str,
) -> Result<Option<MissionDeliveryRow>> {
    conn.query_row(
        "SELECT mission_id, version, snapshot_json, generation_status, curator_model, source_task_ids, source_event_ids, stale, created_at, updated_at \
         FROM mission_deliveries WHERE mission_id = ?1",
        params![mission_id],
        map_mission_delivery_row,
    )
    .optional()
    .map_err(Into::into)
}

pub fn mark_mission_delivery_stale(conn: &Connection, mission_id: &str) -> Result<()> {
    conn.execute(
        "UPDATE mission_deliveries SET stale = 1, updated_at = datetime('now') WHERE mission_id = ?1",
        params![mission_id],
    )?;
    Ok(())
}
```

- [ ] **Step 8: Run query and migration tests**

Run:

```bash
cd src-tauri
cargo test mission_delivery_queries_tests --lib
cargo test migration_creates_handoff_and_delivery_tables --lib
```

Expected: PASS.

- [ ] **Step 9: Commit**

```bash
git add src-tauri/src/db/migrations.rs src-tauri/src/db/queries.rs
git commit -m "feat(delivery): persist mission delivery state" -m "Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

## Task 2: Add delivery domain structs, deterministic handoff fallback, candidate collection, and degraded snapshots

**Files:**
- Create: `src-tauri/src/agent/delivery.rs`
- Modify: `src-tauri/src/agent/mod.rs`

- [ ] **Step 1: Write delivery module tests first**

Create `src-tauri/src/agent/delivery.rs` with only the tests and minimal imports below. The implementation in later steps will fill the missing types/functions.

```rust
use anyhow::Result;
use serde::{Deserialize, Serialize};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_handoff_uses_task_summary_and_artifacts() {
        let packet = TaskHandoffPacket::fallback(
            "m1",
            "t1",
            "Build package",
            "Create release output",
            Some("Built the app and produced a dmg."),
            vec![DeliveryArtifactRef {
                artifact_id: Some("t1.release".into()),
                path: Some("pkg/App.dmg".into()),
                label: "Release dmg".into(),
                purpose: "Installable macOS package".into(),
                how_to_use: Some("Open the dmg and drag the app.".into()),
            }],
        );

        assert_eq!(packet.mission_id, "m1");
        assert_eq!(packet.task_id, "t1");
        assert!(packet.summary.contains("Built the app"));
        assert_eq!(packet.artifacts.len(), 1);
        assert!(packet.downstream_hints.iter().any(|h| h.contains("pkg/App.dmg")));
        assert_eq!(packet.confidence, HandoffConfidence::Medium);
    }

    #[test]
    fn degraded_snapshot_prefers_artifacts_but_never_empty() {
        let snapshot = MissionDeliverySnapshot::degraded(
            "m1",
            "Build macOS app",
            "completed",
            vec![DeliveryCandidate {
                id: "artifact-1".into(),
                path: Some("pkg/App.dmg".into()),
                uri: None,
                label: "App.dmg".into(),
                candidate_kind: "installer".into(),
                source: DeliveryCandidateSource::Artifact,
                evidence: vec!["published artifact".into()],
                size_bytes: Some(42),
                modified_at: None,
            }],
            vec![TaskHandoffPacket::fallback("m1", "t1", "Package", "Package app", Some("Packaged app"), vec![])],
        );

        assert_eq!(snapshot.mission_id, "m1");
        assert_eq!(snapshot.status, DeliveryStatus::CompletedWithWarnings);
        assert_eq!(snapshot.primary_deliverables.len(), 1);
        assert_eq!(snapshot.primary_deliverables[0].path.as_deref(), Some("pkg/App.dmg"));
        assert!(!snapshot.how_to_use.is_empty());
    }

    #[test]
    fn render_handoffs_for_prompt_mentions_direct_context() {
        let packet = TaskHandoffPacket::fallback(
            "m1",
            "t1",
            "Build API",
            "Implement endpoint",
            Some("Added /api/items and tests."),
            vec![],
        );
        let rendered = render_handoffs_for_prompt(&[packet], 2_000);
        assert!(rendered.contains("Upstream Handoff Packets"));
        assert!(rendered.contains("Build API"));
        assert!(rendered.contains("Added /api/items"));
    }
}
```

- [ ] **Step 2: Export module and run tests to verify they fail**

Modify `src-tauri/src/agent/mod.rs`:

```rust
pub mod delivery;
```

Run:

```bash
cd src-tauri
cargo test 'agent::delivery::tests' --lib
```

Expected: FAIL because structs/functions do not exist.

- [ ] **Step 3: Add delivery data model**

Add these definitions above the tests in `src-tauri/src/agent/delivery.rs`.

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HandoffConfidence {
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CommandResultStatus {
    Passed,
    Failed,
    Skipped,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChangedFileSummary {
    pub path: String,
    pub role: String,
    pub change_summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DecisionSummary {
    pub decision: String,
    pub rationale: String,
    #[serde(default)]
    pub alternatives_considered: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommandRunSummary {
    pub command: String,
    pub purpose: String,
    pub result: CommandResultStatus,
    pub evidence: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeliveryArtifactRef {
    pub artifact_id: Option<String>,
    pub path: Option<String>,
    pub label: String,
    pub purpose: String,
    pub how_to_use: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskHandoffPacket {
    pub task_id: String,
    pub mission_id: String,
    pub title: String,
    pub objective: String,
    pub summary: String,
    #[serde(default)]
    pub changed_files: Vec<ChangedFileSummary>,
    #[serde(default)]
    pub decisions: Vec<DecisionSummary>,
    #[serde(default)]
    pub commands_run: Vec<CommandRunSummary>,
    #[serde(default)]
    pub artifacts: Vec<DeliveryArtifactRef>,
    #[serde(default)]
    pub reusable_context: Vec<String>,
    #[serde(default)]
    pub caveats: Vec<String>,
    #[serde(default)]
    pub downstream_hints: Vec<String>,
    pub confidence: HandoffConfidence,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryCandidateSource {
    Artifact,
    Handoff,
    Git,
    Filesystem,
    Manifest,
    ModelHint,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeliveryCandidate {
    pub id: String,
    pub path: Option<String>,
    pub uri: Option<String>,
    pub label: String,
    pub candidate_kind: String,
    pub source: DeliveryCandidateSource,
    #[serde(default)]
    pub evidence: Vec<String>,
    pub size_bytes: Option<u64>,
    pub modified_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryStatus {
    Completed,
    CompletedWithWarnings,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryConfidence {
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeliveryItem {
    pub id: String,
    pub kind: String,
    pub label: String,
    pub path: Option<String>,
    pub uri: Option<String>,
    pub is_primary: bool,
    pub why_this_matters: String,
    pub how_to_use: Option<String>,
    #[serde(default)]
    pub evidence: Vec<String>,
    pub confidence: DeliveryConfidence,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeliveryOverview {
    pub title: String,
    pub summary: String,
    pub user_goal: String,
    pub result: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HowToUseStep {
    pub title: String,
    #[serde(default)]
    pub steps: Vec<String>,
    #[serde(default)]
    pub commands: Vec<String>,
    #[serde(default)]
    pub related_deliverable_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ValidationResultStatus {
    Passed,
    Failed,
    NotRun,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ValidationEvidence {
    pub label: String,
    pub command: Option<String>,
    pub result: ValidationResultStatus,
    pub evidence: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChangeSummary {
    pub label: String,
    pub summary: String,
    #[serde(default)]
    pub files: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MissionDeliverySnapshot {
    pub mission_id: String,
    pub generated_at: String,
    pub status: DeliveryStatus,
    pub overview: DeliveryOverview,
    #[serde(default)]
    pub primary_deliverables: Vec<DeliveryItem>,
    #[serde(default)]
    pub supporting_deliverables: Vec<DeliveryItem>,
    #[serde(default)]
    pub how_to_use: Vec<HowToUseStep>,
    #[serde(default)]
    pub validation: Vec<ValidationEvidence>,
    #[serde(default)]
    pub changes: Vec<ChangeSummary>,
    #[serde(default)]
    pub caveats: Vec<String>,
    #[serde(default)]
    pub next_steps: Vec<String>,
    pub report_id: Option<String>,
}
```

- [ ] **Step 4: Add fallback and render helpers**

Add these implementations below the structs.

```rust
impl TaskHandoffPacket {
    pub fn fallback(
        mission_id: impl Into<String>,
        task_id: impl Into<String>,
        title: impl Into<String>,
        objective: impl Into<String>,
        summary: Option<&str>,
        artifacts: Vec<DeliveryArtifactRef>,
    ) -> Self {
        let title = title.into();
        let summary_text = summary
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("Task completed; no detailed summary was provided.")
            .to_string();
        let downstream_hints = artifacts
            .iter()
            .filter_map(|artifact| artifact.path.as_ref().map(|path| format!("Reuse artifact '{}' at {}.", artifact.label, path)))
            .collect();
        Self {
            task_id: task_id.into(),
            mission_id: mission_id.into(),
            title,
            objective: objective.into(),
            summary: summary_text,
            changed_files: Vec::new(),
            decisions: Vec::new(),
            commands_run: Vec::new(),
            artifacts,
            reusable_context: Vec::new(),
            caveats: vec!["Fallback handoff generated from completion summary and artifacts.".into()],
            downstream_hints,
            confidence: HandoffConfidence::Medium,
        }
    }
}

impl MissionDeliverySnapshot {
    pub fn degraded(
        mission_id: impl Into<String>,
        mission_title: impl Into<String>,
        mission_status: &str,
        candidates: Vec<DeliveryCandidate>,
        handoffs: Vec<TaskHandoffPacket>,
    ) -> Self {
        let mission_id = mission_id.into();
        let mission_title = mission_title.into();
        let primary = candidates.first().map(candidate_to_primary_item);
        let primary_deliverables = primary.into_iter().collect::<Vec<_>>();
        let supporting_deliverables = candidates
            .iter()
            .skip(1)
            .map(candidate_to_supporting_item)
            .collect::<Vec<_>>();
        let status = match mission_status {
            "failed" => DeliveryStatus::Failed,
            "completed" if primary_deliverables.is_empty() => DeliveryStatus::CompletedWithWarnings,
            "completed" => DeliveryStatus::CompletedWithWarnings,
            _ => DeliveryStatus::CompletedWithWarnings,
        };
        let mut caveats = vec!["Delivery snapshot was generated from deterministic fallback data; model curation was not available.".into()];
        if primary_deliverables.is_empty() {
            caveats.push("No explicit final deliverable was identified. Review the source project, artifacts, and follow-up steps.".into());
        }
        let how_to_use = if let Some(item) = primary_deliverables.first() {
            vec![HowToUseStep {
                title: format!("Use {}", item.label),
                steps: vec![item.how_to_use.clone().unwrap_or_else(|| "Open or inspect the listed path/URI.".into())],
                commands: Vec::new(),
                related_deliverable_ids: vec![item.id.clone()],
            }]
        } else {
            vec![HowToUseStep {
                title: "Review delivered project state".into(),
                steps: vec!["Open the repository, inspect completed task handoffs, and use follow-up chat for packaging or validation.".into()],
                commands: Vec::new(),
                related_deliverable_ids: Vec::new(),
            }]
        };
        Self {
            mission_id,
            generated_at: "".into(),
            status,
            overview: DeliveryOverview {
                title: mission_title.clone(),
                summary: summarize_handoffs(&handoffs),
                user_goal: mission_title,
                result: if primary_deliverables.is_empty() { "Completed with no explicit packaged deliverable identified.".into() } else { "Completed with candidate deliverables identified.".into() },
            },
            primary_deliverables,
            supporting_deliverables,
            how_to_use,
            validation: Vec::new(),
            changes: handoffs
                .iter()
                .map(|h| ChangeSummary { label: h.title.clone(), summary: h.summary.clone(), files: h.changed_files.iter().map(|f| f.path.clone()).collect() })
                .collect(),
            caveats,
            next_steps: vec!["Use follow-up chat to ask for packaging, verification, or refinements.".into()],
            report_id: None,
        }
    }
}

fn candidate_to_primary_item(candidate: &DeliveryCandidate) -> DeliveryItem {
    DeliveryItem {
        id: candidate.id.clone(),
        kind: candidate.candidate_kind.clone(),
        label: candidate.label.clone(),
        path: candidate.path.clone(),
        uri: candidate.uri.clone(),
        is_primary: true,
        why_this_matters: "Best available delivery candidate from artifacts and mission output.".into(),
        how_to_use: Some("Open, run, install, or inspect this deliverable according to the mission context.".into()),
        evidence: candidate.evidence.clone(),
        confidence: DeliveryConfidence::Medium,
        warnings: Vec::new(),
    }
}

fn candidate_to_supporting_item(candidate: &DeliveryCandidate) -> DeliveryItem {
    let mut item = candidate_to_primary_item(candidate);
    item.is_primary = false;
    item.why_this_matters = "Supporting delivery candidate.".into();
    item
}

fn summarize_handoffs(handoffs: &[TaskHandoffPacket]) -> String {
    if handoffs.is_empty() {
        return "Mission reached a terminal state. No task handoff packets were available.".into();
    }
    handoffs
        .iter()
        .map(|h| format!("{}: {}", h.title, h.summary))
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn render_handoffs_for_prompt(handoffs: &[TaskHandoffPacket], max_chars: usize) -> String {
    if handoffs.is_empty() {
        return String::new();
    }
    let mut out = String::from("## Upstream Handoff Packets\n\nUse this as authoritative context from completed upstream tasks. Do not rediscover work that is already summarized unless validation requires it.\n");
    for handoff in handoffs {
        out.push_str(&format!(
            "\n### {}\nObjective: {}\nSummary: {}\n",
            handoff.title, handoff.objective, handoff.summary
        ));
        if !handoff.changed_files.is_empty() {
            out.push_str("Changed files:\n");
            for file in &handoff.changed_files {
                out.push_str(&format!("- {} — {}\n", file.path, file.change_summary));
            }
        }
        if !handoff.decisions.is_empty() {
            out.push_str("Decisions:\n");
            for decision in &handoff.decisions {
                out.push_str(&format!("- {} — {}\n", decision.decision, decision.rationale));
            }
        }
        if !handoff.caveats.is_empty() {
            out.push_str("Caveats:\n");
            for caveat in &handoff.caveats {
                out.push_str(&format!("- {}\n", caveat));
            }
        }
        if !handoff.downstream_hints.is_empty() {
            out.push_str("Downstream hints:\n");
            for hint in &handoff.downstream_hints {
                out.push_str(&format!("- {}\n", hint));
            }
        }
    }
    truncate_chars(out, max_chars)
}

pub fn render_delivery_for_prompt(snapshot: &MissionDeliverySnapshot, max_chars: usize) -> String {
    let mut out = format!(
        "## Mission Delivery Snapshot\n\nResult: {}\nSummary: {}\n",
        snapshot.overview.result, snapshot.overview.summary
    );
    if !snapshot.primary_deliverables.is_empty() {
        out.push_str("Primary deliverables:\n");
        for item in &snapshot.primary_deliverables {
            out.push_str(&format!("- {} ({})", item.label, item.kind));
            if let Some(path) = &item.path {
                out.push_str(&format!(" @ {}", path));
            }
            out.push('\n');
        }
    }
    if !snapshot.caveats.is_empty() {
        out.push_str("Caveats:\n");
        for caveat in &snapshot.caveats {
            out.push_str(&format!("- {}\n", caveat));
        }
    }
    truncate_chars(out, max_chars)
}

fn truncate_chars(mut value: String, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value;
    }
    value = value.chars().take(max_chars.saturating_sub(20)).collect();
    value.push_str("\n…[truncated]");
    value
}
```

- [ ] **Step 5: Run delivery module tests**

Run:

```bash
cd src-tauri
cargo test 'agent::delivery::tests' --lib
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/agent/delivery.rs src-tauri/src/agent/mod.rs
git commit -m "feat(delivery): add delivery domain model" -m "Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

## Task 3: Persist task handoff packets from task completion

**Files:**
- Modify: `src-tauri/src/tools/definitions.rs`
- Modify: `src-tauri/src/agent/engine.rs`
- Modify: `src-tauri/src/agent/delivery.rs`

- [ ] **Step 1: Add parsing test for agent-authored handoff**

Add to `src-tauri/src/agent/delivery.rs` tests:

```rust
#[test]
fn parses_agent_authored_handoff_from_task_complete_input() {
    let input = serde_json::json!({
        "summary": "Implemented packaging",
        "handoff": {
            "changed_files": [
                {"path": "package.json", "role": "build config", "change_summary": "Added bundle script"}
            ],
            "decisions": [
                {"decision": "Use dmg output", "rationale": "Best macOS delivery format"}
            ],
            "commands_run": [
                {"command": "pnpm bundle", "purpose": "Build release", "result": "passed", "evidence": "created pkg/App.dmg"}
            ],
            "reusable_context": ["Bundle script stages output under pkg/"],
            "caveats": ["Not notarized"],
            "downstream_hints": ["Use pkg/App.dmg as primary deliverable"],
            "confidence": "high"
        }
    });

    let handoff = TaskHandoffPacket::from_task_complete_value(
        "m1",
        "t1",
        "Package app",
        "Create release package",
        &input,
        vec![],
    )
    .expect("parse handoff");

    assert_eq!(handoff.confidence, HandoffConfidence::High);
    assert_eq!(handoff.changed_files[0].path, "package.json");
    assert_eq!(handoff.commands_run[0].result, CommandResultStatus::Passed);
}
```

- [ ] **Step 2: Run the parsing test to verify it fails**

Run:

```bash
cd src-tauri
cargo test parses_agent_authored_handoff_from_task_complete_input --lib
```

Expected: FAIL because `from_task_complete_value` is missing.

- [ ] **Step 3: Implement `from_task_complete_value`**

Add to `src-tauri/src/agent/delivery.rs`:

```rust
#[derive(Debug, Deserialize, Default)]
struct TaskCompleteHandoffInput {
    #[serde(default)]
    changed_files: Vec<ChangedFileSummary>,
    #[serde(default)]
    decisions: Vec<DecisionSummary>,
    #[serde(default)]
    commands_run: Vec<CommandRunSummary>,
    #[serde(default)]
    artifacts: Vec<DeliveryArtifactRef>,
    #[serde(default)]
    reusable_context: Vec<String>,
    #[serde(default)]
    caveats: Vec<String>,
    #[serde(default)]
    downstream_hints: Vec<String>,
    confidence: Option<HandoffConfidence>,
}

impl TaskHandoffPacket {
    pub fn from_task_complete_value(
        mission_id: &str,
        task_id: &str,
        title: &str,
        objective: &str,
        value: &serde_json::Value,
        published_artifacts: Vec<DeliveryArtifactRef>,
    ) -> Result<Self> {
        let summary = value
            .get("summary")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("Task completed; no summary was provided.");

        let Some(handoff_value) = value.get("handoff") else {
            return Ok(Self::fallback(mission_id, task_id, title, objective, Some(summary), published_artifacts));
        };

        let input: TaskCompleteHandoffInput = serde_json::from_value(handoff_value.clone())?;
        let mut artifacts = input.artifacts;
        artifacts.extend(published_artifacts);
        Ok(Self {
            task_id: task_id.to_string(),
            mission_id: mission_id.to_string(),
            title: title.to_string(),
            objective: objective.to_string(),
            summary: summary.to_string(),
            changed_files: input.changed_files,
            decisions: input.decisions,
            commands_run: input.commands_run,
            artifacts,
            reusable_context: input.reusable_context,
            caveats: input.caveats,
            downstream_hints: input.downstream_hints,
            confidence: input.confidence.unwrap_or(HandoffConfidence::Medium),
        })
    }
}
```

- [ ] **Step 4: Run parsing test**

Run:

```bash
cd src-tauri
cargo test parses_agent_authored_handoff_from_task_complete_input --lib
```

Expected: PASS.

- [ ] **Step 5: Extend task_complete tool schema**

In `src-tauri/src/tools/definitions.rs`, find `task_complete_tool_definition()`. Add an optional `handoff` object to the tool input schema. The exact surrounding JSON may differ; the resulting schema must include this property:

```rust
"handoff": {
    "type": "object",
    "description": "Structured handoff packet for downstream agents and mission delivery. Use this to explain what changed, why, how it was validated, caveats, and what downstream agents should reuse.",
    "properties": {
        "changed_files": {
            "type": "array",
            "items": {
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "role": { "type": "string" },
                    "change_summary": { "type": "string" }
                },
                "required": ["path", "role", "change_summary"]
            }
        },
        "decisions": {
            "type": "array",
            "items": {
                "type": "object",
                "properties": {
                    "decision": { "type": "string" },
                    "rationale": { "type": "string" },
                    "alternatives_considered": { "type": "array", "items": { "type": "string" } }
                },
                "required": ["decision", "rationale"]
            }
        },
        "commands_run": {
            "type": "array",
            "items": {
                "type": "object",
                "properties": {
                    "command": { "type": "string" },
                    "purpose": { "type": "string" },
                    "result": { "type": "string", "enum": ["passed", "failed", "skipped", "unknown"] },
                    "evidence": { "type": "string" }
                },
                "required": ["command", "purpose", "result"]
            }
        },
        "artifacts": { "type": "array", "items": { "type": "object" } },
        "reusable_context": { "type": "array", "items": { "type": "string" } },
        "caveats": { "type": "array", "items": { "type": "string" } },
        "downstream_hints": { "type": "array", "items": { "type": "string" } },
        "confidence": { "type": "string", "enum": ["high", "medium", "low"] }
    }
}
```

Keep `summary` as the only required field unless existing guardrails require otherwise.

- [ ] **Step 6: Persist handoff in engine completion path**

In `src-tauri/src/agent/engine.rs`, find the branch handling `CompletionOutcome::Completed` after `persist_completion_summary(...)`. Add a helper call similar to this, adapting names to actual local variables in the function:

```rust
if let Err(err) = self.persist_task_handoff_packet(
    &options.mission_id,
    task_id,
    &options.task_title,
    &options.task_description,
    &tool_input_value,
)
.await {
    tracing::warn!(task_id = %task_id, error = %err, "failed to persist task handoff packet");
}
```

Add a private method to `impl AgentEngine`:

```rust
async fn persist_task_handoff_packet(
    &self,
    mission_id: &str,
    task_id: &str,
    task_title: &str,
    task_description: &str,
    task_complete_input: &serde_json::Value,
) -> anyhow::Result<()> {
    let packet = crate::agent::delivery::TaskHandoffPacket::from_task_complete_value(
        mission_id,
        task_id,
        task_title,
        task_description,
        task_complete_input,
        Vec::new(),
    )?;
    let packet_json = serde_json::to_string(&packet)?;
    let status = if task_complete_input.get("handoff").is_some() {
        "agent_authored"
    } else {
        "fallback"
    };
    self.db.with_conn(|conn| {
        crate::db::queries::upsert_task_handoff_packet(
            conn,
            task_id,
            mission_id,
            &packet_json,
            status,
        )
    })?;
    Ok(())
}
```

If `AgentRunOptions` does not expose `mission_id`, `task_title`, or `task_description` at this location, derive them by querying the `tasks` row inside the helper. Do not duplicate scheduler status writes.

- [ ] **Step 7: Run focused Rust checks**

Run:

```bash
cd src-tauri
cargo test parses_agent_authored_handoff_from_task_complete_input --lib
cargo check
```

Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add src-tauri/src/tools/definitions.rs src-tauri/src/agent/engine.rs src-tauri/src/agent/delivery.rs
git commit -m "feat(delivery): persist task handoffs" -m "Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

## Task 4: Inject upstream handoff packets into coding-agent prompts

**Files:**
- Modify: `src-tauri/src/agent/codebase_intel.rs`
- Modify: `src-tauri/src/agent/delivery.rs`

- [ ] **Step 1: Add prompt rendering test**

In `src-tauri/src/agent/codebase_intel.rs`, add a test near existing tests or at the bottom:

```rust
#[cfg(test)]
mod delivery_handoff_prompt_tests {
    use super::*;
    use crate::agent::delivery::{HandoffConfidence, TaskHandoffPacket};

    #[test]
    fn render_system_block_includes_task_handoffs_after_upstream_context() {
        let intel = CodebaseIntel {
            project_structure: String::new(),
            tech_stack: String::new(),
            upstream_context: "legacy upstream".into(),
            upstream_handoffs: crate::agent::delivery::render_handoffs_for_prompt(
                &[TaskHandoffPacket {
                    task_id: "t1".into(),
                    mission_id: "m1".into(),
                    title: "Parent task".into(),
                    objective: "Prepare context".into(),
                    summary: "Found the key API.".into(),
                    changed_files: vec![],
                    decisions: vec![],
                    commands_run: vec![],
                    artifacts: vec![],
                    reusable_context: vec![],
                    caveats: vec![],
                    downstream_hints: vec![],
                    confidence: HandoffConfidence::High,
                }],
                2_000,
            ),
            base_conflicts: String::new(),
        };
        let rendered = intel.render_system_block();
        assert!(rendered.contains("[Upstream Context]"));
        assert!(rendered.contains("legacy upstream"));
        assert!(rendered.contains("[Upstream Handoff Packets]"));
        assert!(rendered.contains("Found the key API"));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run:

```bash
cd src-tauri
cargo test render_system_block_includes_task_handoffs_after_upstream_context --lib
```

Expected: FAIL because `CodebaseIntel.upstream_handoffs` is missing.

- [ ] **Step 3: Add handoff field and render section**

Modify `CodebaseIntel` in `src-tauri/src/agent/codebase_intel.rs`:

```rust
#[derive(Debug, Clone, Default, Serialize)]
pub struct CodebaseIntel {
    pub project_structure: String,
    pub tech_stack: String,
    pub upstream_context: String,
    pub upstream_handoffs: String,
    pub base_conflicts: String,
}
```

In `render_system_block`, after `[Upstream Context]`, add:

```rust
if !self.upstream_handoffs.trim().is_empty() {
    out.push_str("\n\n[Upstream Handoff Packets]\n");
    out.push_str(self.upstream_handoffs.trim_end());
}
```

In `build_intel`, fetch and set `upstream_handoffs`:

```rust
let (upstream_context, upstream_handoffs, base_conflicts) = match (task_id, db) {
    (Some(tid), Some(db)) => {
        let upstream = build_upstream_context(db, tid);
        let handoffs = build_upstream_handoff_context(db, tid);
        let conflicts = build_base_conflicts(db, tid);
        (upstream, handoffs, conflicts)
    }
    _ => (String::new(), String::new(), String::new()),
};

CodebaseIntel {
    project_structure: truncate_block(&project_structure, PROJECT_TREE_BUDGET_CHARS),
    tech_stack: truncate_block(&tech_stack, TECH_STACK_BUDGET_CHARS),
    upstream_context: truncate_block(&upstream_context, UPSTREAM_BUDGET_CHARS),
    upstream_handoffs: truncate_block(&upstream_handoffs, UPSTREAM_BUDGET_CHARS),
    base_conflicts: truncate_block(&base_conflicts, BASE_CONFLICTS_BUDGET_CHARS),
}
```

Add helper:

```rust
fn build_upstream_handoff_context(db: &crate::db::Database, task_id: &str) -> String {
    let rows = match db.with_conn(|conn| crate::db::queries::list_parent_handoff_packets(conn, task_id)) {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(task_id, error = %e, "upstream handoff fetch failed");
            return String::new();
        }
    };
    let packets = rows
        .into_iter()
        .filter_map(|row| match serde_json::from_str::<crate::agent::delivery::TaskHandoffPacket>(&row.packet_json) {
            Ok(packet) => Some(packet),
            Err(e) => {
                tracing::warn!(task_id = %row.task_id, error = %e, "invalid task handoff packet json");
                None
            }
        })
        .collect::<Vec<_>>();
    crate::agent::delivery::render_handoffs_for_prompt(&packets, UPSTREAM_BUDGET_CHARS)
}
```

- [ ] **Step 4: Run focused test**

Run:

```bash
cd src-tauri
cargo test render_system_block_includes_task_handoffs_after_upstream_context --lib
```

Expected: PASS.

- [ ] **Step 5: Run broader codebase intel tests/check**

Run:

```bash
cd src-tauri
cargo test codebase_intel --lib
cargo check
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/agent/codebase_intel.rs src-tauri/src/agent/delivery.rs
git commit -m "feat(delivery): inject upstream handoffs" -m "Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

## Task 5: Generate and persist mission delivery snapshots on demand

**Files:**
- Modify: `src-tauri/src/agent/delivery.rs`
- Create: `src-tauri/src/commands/delivery.rs`
- Modify: `src-tauri/src/commands/mod.rs`
- Modify: `src-tauri/src/lib.rs`
- Modify: `src/ipc/commands.ts`

- [ ] **Step 1: Add backend command tests or pure generator tests first**

Add this test to `src-tauri/src/agent/delivery.rs`:

```rust
#[test]
fn delivery_snapshot_round_trips_json_for_frontend() {
    let snapshot = MissionDeliverySnapshot::degraded(
        "m1",
        "Build app",
        "completed",
        vec![],
        vec![],
    );
    let json = serde_json::to_string(&snapshot).expect("serialize snapshot");
    assert!(json.contains("primary_deliverables"));
    let parsed: MissionDeliverySnapshot = serde_json::from_str(&json).expect("deserialize snapshot");
    assert_eq!(parsed.mission_id, "m1");
}
```

- [ ] **Step 2: Run test**

Run:

```bash
cd src-tauri
cargo test delivery_snapshot_round_trips_json_for_frontend --lib
```

Expected: PASS after Task 2 exists. If it fails because enum casing differs, adjust serde names now and keep frontend types aligned.

- [ ] **Step 3: Implement deterministic candidate collection from artifacts and handoffs**

Add in `src-tauri/src/agent/delivery.rs`:

```rust
pub fn candidates_from_artifacts(artifacts: &[crate::db::queries::ArtifactRow]) -> Vec<DeliveryCandidate> {
    artifacts
        .iter()
        .flat_map(|artifact| {
            let paths = serde_json::from_str::<Vec<String>>(&artifact.file_paths).unwrap_or_default();
            if paths.is_empty() {
                vec![DeliveryCandidate {
                    id: format!("artifact:{}", artifact.id),
                    path: None,
                    uri: None,
                    label: artifact.local_name.clone(),
                    candidate_kind: artifact.r#type.clone(),
                    source: DeliveryCandidateSource::Artifact,
                    evidence: vec![artifact.summary.clone()],
                    size_bytes: None,
                    modified_at: None,
                }]
            } else {
                paths
                    .into_iter()
                    .enumerate()
                    .map(|(idx, path)| DeliveryCandidate {
                        id: format!("artifact:{}:{}", artifact.id, idx),
                        path: Some(path),
                        uri: None,
                        label: artifact.local_name.clone(),
                        candidate_kind: artifact.r#type.clone(),
                        source: DeliveryCandidateSource::Artifact,
                        evidence: vec![artifact.summary.clone()],
                        size_bytes: None,
                        modified_at: None,
                    })
                    .collect()
            }
        })
        .collect()
}
```

If `ArtifactRow` uses `artifact_type` instead of `r#type`, use the actual field name from `queries.rs`.

- [ ] **Step 4: Add generation entrypoint**

Add in `src-tauri/src/agent/delivery.rs`:

```rust
pub fn generate_degraded_delivery_snapshot(
    conn: &rusqlite::Connection,
    mission_id: &str,
) -> Result<MissionDeliverySnapshot> {
    let (title, status): (String, String) = conn.query_row(
        "SELECT title, status FROM missions WHERE id = ?1",
        rusqlite::params![mission_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let handoff_rows = crate::db::queries::list_task_handoff_packets_for_mission(conn, mission_id)?;
    let handoffs = handoff_rows
        .into_iter()
        .filter_map(|row| serde_json::from_str::<TaskHandoffPacket>(&row.packet_json).ok())
        .collect::<Vec<_>>();
    let artifacts = crate::db::queries::list_artifacts_for_mission(conn, mission_id)?;
    let candidates = candidates_from_artifacts(&artifacts);
    Ok(MissionDeliverySnapshot::degraded(mission_id, title, &status, candidates, handoffs))
}

pub fn persist_delivery_snapshot(
    conn: &rusqlite::Connection,
    snapshot: &MissionDeliverySnapshot,
    generation_status: &str,
    curator_model: Option<&str>,
) -> Result<()> {
    let snapshot_json = serde_json::to_string(snapshot)?;
    let source_task_ids = serde_json::to_string(
        &snapshot
            .changes
            .iter()
            .map(|change| change.label.clone())
            .collect::<Vec<_>>(),
    )?;
    crate::db::queries::upsert_mission_delivery(
        conn,
        &snapshot.mission_id,
        1,
        &snapshot_json,
        generation_status,
        curator_model,
        &source_task_ids,
        "[]",
        false,
    )?;
    Ok(())
}
```

- [ ] **Step 5: Create delivery IPC commands**

Create `src-tauri/src/commands/delivery.rs`:

```rust
use crate::agent::delivery::MissionDeliverySnapshot;
use crate::db::{queries, Database};
use serde::Serialize;
use tauri::Manager;

#[derive(Debug, Serialize, Clone)]
pub struct MissionDeliveryView {
    pub mission_id: String,
    pub version: i64,
    pub generated_at: String,
    pub generation_status: String,
    pub curator_model: Option<String>,
    pub stale: bool,
    pub snapshot: MissionDeliverySnapshot,
}

#[derive(Debug, Serialize, Clone)]
pub struct GenerateMissionDeliveryResponse {
    pub mission_id: String,
    pub generation_status: String,
}

#[tauri::command]
pub fn get_mission_delivery(
    app: tauri::AppHandle,
    mission_id: String,
) -> Result<Option<MissionDeliveryView>, String> {
    let db = app.state::<Database>();
    db.with_conn(|conn| {
        let Some(row) = queries::get_mission_delivery(conn, &mission_id)? else {
            return Ok(None);
        };
        let snapshot = serde_json::from_str::<MissionDeliverySnapshot>(&row.snapshot_json)
            .map_err(|e| anyhow::anyhow!("corrupt mission delivery snapshot: {e}"))?;
        Ok(Some(MissionDeliveryView {
            mission_id: row.mission_id,
            version: row.version,
            generated_at: row.updated_at,
            generation_status: row.generation_status,
            curator_model: row.curator_model,
            stale: row.stale,
            snapshot,
        }))
    })
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn generate_mission_delivery(
    app: tauri::AppHandle,
    mission_id: String,
) -> Result<GenerateMissionDeliveryResponse, String> {
    let db = app.state::<Database>();
    db.with_conn(|conn| {
        let snapshot = crate::agent::delivery::generate_degraded_delivery_snapshot(conn, &mission_id)?;
        crate::agent::delivery::persist_delivery_snapshot(conn, &snapshot, "degraded", Some("deterministic-fallback"))?;
        Ok(GenerateMissionDeliveryResponse {
            mission_id,
            generation_status: "degraded".into(),
        })
    })
    .map_err(|e| e.to_string())
}
```

- [ ] **Step 6: Register commands**

In `src-tauri/src/commands/mod.rs`:

```rust
mod delivery;
pub use delivery::*;
```

In `src-tauri/src/lib.rs`, add to `tauri::generate_handler!`:

```rust
commands::get_mission_delivery,
commands::generate_mission_delivery,
```

- [ ] **Step 7: Add frontend IPC types and wrappers**

In `src/ipc/commands.ts`, add near report types:

```ts
export type DeliveryStatus = "completed" | "completed_with_warnings" | "failed";
export type DeliveryConfidence = "high" | "medium" | "low";
export type ValidationResultStatus = "passed" | "failed" | "not_run" | "unknown";

export interface DeliveryItem {
  id: string;
  kind: string;
  label: string;
  path?: string | null;
  uri?: string | null;
  is_primary: boolean;
  why_this_matters: string;
  how_to_use?: string | null;
  evidence: string[];
  confidence: DeliveryConfidence;
  warnings: string[];
}

export interface MissionDeliverySnapshot {
  mission_id: string;
  generated_at: string;
  status: DeliveryStatus;
  overview: {
    title: string;
    summary: string;
    user_goal: string;
    result: string;
  };
  primary_deliverables: DeliveryItem[];
  supporting_deliverables: DeliveryItem[];
  how_to_use: Array<{
    title: string;
    steps: string[];
    commands: string[];
    related_deliverable_ids: string[];
  }>;
  validation: Array<{
    label: string;
    command?: string | null;
    result: ValidationResultStatus;
    evidence: string;
  }>;
  changes: Array<{
    label: string;
    summary: string;
    files: string[];
  }>;
  caveats: string[];
  next_steps: string[];
  report_id?: string | null;
}

export interface MissionDeliveryView {
  mission_id: string;
  version: number;
  generated_at: string;
  generation_status: "generated" | "degraded" | "failed";
  curator_model?: string | null;
  stale: boolean;
  snapshot: MissionDeliverySnapshot;
}

export interface GenerateMissionDeliveryResponse {
  mission_id: string;
  generation_status: "generated" | "degraded" | "failed";
}
```

Add command wrappers in `commands`:

```ts
getMissionDelivery: (missionId: string) =>
  invoke<MissionDeliveryView | null>("get_mission_delivery", { missionId }),

generateMissionDelivery: (missionId: string) =>
  invoke<GenerateMissionDeliveryResponse>("generate_mission_delivery", { missionId }),
```

- [ ] **Step 8: Run checks**

Run:

```bash
cd src-tauri
cargo check
cd ..
pnpm tsc --noEmit
```

Expected: PASS.

- [ ] **Step 9: Commit**

```bash
git add src-tauri/src/agent/delivery.rs src-tauri/src/commands/delivery.rs src-tauri/src/commands/mod.rs src-tauri/src/lib.rs src/ipc/commands.ts
git commit -m "feat(delivery): expose mission delivery snapshots" -m "Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

## Task 6: Trigger delivery generation from mission terminal flow

**Files:**
- Modify: `src-tauri/src/agent/scheduler.rs`
- Modify: `src-tauri/src/agent/delivery.rs`

- [ ] **Step 1: Add helper that is safe to call from scheduler**

Add to `src-tauri/src/agent/delivery.rs`:

```rust
pub fn generate_and_persist_degraded_delivery(
    db: &crate::db::Database,
    mission_id: &str,
) -> Result<()> {
    db.with_conn(|conn| {
        let snapshot = generate_degraded_delivery_snapshot(conn, mission_id)?;
        persist_delivery_snapshot(conn, &snapshot, "degraded", Some("deterministic-fallback"))?;
        Ok(())
    })
}
```

- [ ] **Step 2: Run cargo check**

Run:

```bash
cd src-tauri
cargo check
```

Expected: PASS.

- [ ] **Step 3: Wire scheduler best-effort generation**

In `src-tauri/src/agent/scheduler.rs`, find where `poll_and_dispatch(...)` handles terminal mission status and emits `mission-status-changed`. Add a best-effort call when status is `completed` or `failed`.

Use this pattern, adapting variable names to the existing function:

```rust
if matches!(new_status.as_str(), "completed" | "failed") {
    let mission_id_for_delivery = mission_id.to_string();
    let app_for_delivery = app.clone();
    tauri::async_runtime::spawn(async move {
        let Some(db) = app_for_delivery.try_state::<crate::db::Database>() else {
            tracing::warn!(mission_id = %mission_id_for_delivery, "database state missing; cannot generate delivery snapshot");
            return;
        };
        if let Err(err) = crate::agent::delivery::generate_and_persist_degraded_delivery(&db, &mission_id_for_delivery) {
            tracing::warn!(mission_id = %mission_id_for_delivery, error = %err, "mission delivery generation failed");
        }
    });
}
```

If the scheduler already has a post-merge completion hook, prefer calling this after final merge completes so candidates include merged artifacts. Keep the call non-blocking and best-effort; do not prevent terminal mission status.

- [ ] **Step 4: Run Rust checks**

Run:

```bash
cd src-tauri
cargo check
cargo test 'agent::delivery::tests' --lib
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/agent/scheduler.rs src-tauri/src/agent/delivery.rs
git commit -m "feat(delivery): generate snapshots on completion" -m "Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

## Task 7: Add Delivery Workspace UI and completed-state integration

**Files:**
- Create: `src/components/mission/DeliveryWorkspace.tsx`
- Create: `src/components/mission/DeliveryWorkspace.module.css`
- Create: `src/components/mission/DeliveryWorkspace.test.tsx`
- Modify: `src/views/MissionsView.tsx`
- Modify: `src/i18n/locales/en-US.json`
- Modify: `src/i18n/locales/zh-CN.json`

- [ ] **Step 1: Write component test first**

Create `src/components/mission/DeliveryWorkspace.test.tsx`:

```tsx
import { render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { DeliveryWorkspace } from "./DeliveryWorkspace";
import { commands } from "../../ipc/commands";

vi.mock("../../ipc/commands", () => ({
  commands: {
    getMissionDelivery: vi.fn(),
    generateMissionDelivery: vi.fn(),
    openInFinder: vi.fn(),
  },
}));

vi.mock("./MissionChatPanel", () => ({
  MissionChatPanel: ({ missionId }: { missionId: string }) => (
    <div data-testid="mission-chat">chat:{missionId}</div>
  ),
}));

const mission = {
  id: "m1",
  title: "Build macOS app",
  description: "Create a packaged app",
  status: "completed" as const,
  created_at: "2026-06-12",
  updated_at: "2026-06-12",
  total_cost_usd: 0,
};

describe("DeliveryWorkspace", () => {
  it("renders primary delivery and follow-up chat from persisted snapshot", async () => {
    vi.mocked(commands.getMissionDelivery).mockResolvedValue({
      mission_id: "m1",
      version: 1,
      generated_at: "2026-06-12",
      generation_status: "generated",
      curator_model: "deterministic-fallback",
      stale: false,
      snapshot: {
        mission_id: "m1",
        generated_at: "2026-06-12",
        status: "completed_with_warnings",
        overview: {
          title: "Build macOS app",
          summary: "Packaged the app.",
          user_goal: "Create a packaged app",
          result: "Completed with candidate deliverables identified.",
        },
        primary_deliverables: [
          {
            id: "dmg",
            kind: "installer",
            label: "App.dmg",
            path: "pkg/App.dmg",
            uri: null,
            is_primary: true,
            why_this_matters: "Installable package",
            how_to_use: "Open it",
            evidence: ["published artifact"],
            confidence: "medium",
            warnings: [],
          },
        ],
        supporting_deliverables: [],
        how_to_use: [{ title: "Install", steps: ["Open dmg"], commands: [], related_deliverable_ids: ["dmg"] }],
        validation: [],
        changes: [],
        caveats: ["Not notarized"],
        next_steps: ["Ask for notarization"],
        report_id: null,
      },
    });

    render(<DeliveryWorkspace mission={mission} onFollowupCreated={vi.fn()} />);

    expect(screen.getByText(/Preparing delivery summary/i)).toBeInTheDocument();
    await waitFor(() => expect(screen.getByText("App.dmg")).toBeInTheDocument());
    expect(screen.getByText("pkg/App.dmg")).toBeInTheDocument();
    expect(screen.getByTestId("mission-chat")).toHaveTextContent("chat:m1");
  });

  it("generates a snapshot when none exists", async () => {
    vi.mocked(commands.getMissionDelivery).mockResolvedValueOnce(null).mockResolvedValueOnce({
      mission_id: "m1",
      version: 1,
      generated_at: "2026-06-12",
      generation_status: "degraded",
      curator_model: "deterministic-fallback",
      stale: false,
      snapshot: {
        mission_id: "m1",
        generated_at: "2026-06-12",
        status: "completed_with_warnings",
        overview: { title: "Build macOS app", summary: "No package found", user_goal: "Create app", result: "No explicit packaged deliverable identified." },
        primary_deliverables: [],
        supporting_deliverables: [],
        how_to_use: [],
        validation: [],
        changes: [],
        caveats: ["No explicit final deliverable was identified."],
        next_steps: [],
        report_id: null,
      },
    });
    vi.mocked(commands.generateMissionDelivery).mockResolvedValue({ mission_id: "m1", generation_status: "degraded" });

    render(<DeliveryWorkspace mission={mission} onFollowupCreated={vi.fn()} />);

    await waitFor(() => expect(commands.generateMissionDelivery).toHaveBeenCalledWith("m1"));
    await waitFor(() => expect(screen.getByText(/No explicit packaged deliverable/i)).toBeInTheDocument());
  });
});
```

If `MissionInfo` uses camelCase fields instead of snake_case, adjust the test object to the actual frontend type.

- [ ] **Step 2: Run test to verify it fails**

Run:

```bash
pnpm test src/components/mission/DeliveryWorkspace.test.tsx
```

Expected: FAIL because the component does not exist.

- [ ] **Step 3: Implement DeliveryWorkspace component**

Create `src/components/mission/DeliveryWorkspace.tsx`:

```tsx
import { useEffect, useMemo, useState } from "react";
import { commands, MissionDeliveryView, MissionInfo } from "../../ipc/commands";
import { MissionChatPanel } from "./MissionChatPanel";
import styles from "./DeliveryWorkspace.module.css";

interface DeliveryWorkspaceProps {
  mission: MissionInfo;
  onFollowupCreated?: (childMissionId: string) => void;
}

export function DeliveryWorkspace({ mission, onFollowupCreated }: DeliveryWorkspaceProps) {
  const [delivery, setDelivery] = useState<MissionDeliveryView | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    async function load() {
      setLoading(true);
      setError(null);
      try {
        let view = await commands.getMissionDelivery(mission.id);
        if (!view) {
          await commands.generateMissionDelivery(mission.id);
          view = await commands.getMissionDelivery(mission.id);
        }
        if (!cancelled) {
          setDelivery(view);
        }
      } catch (err) {
        if (!cancelled) {
          setError(err instanceof Error ? err.message : String(err));
        }
      } finally {
        if (!cancelled) {
          setLoading(false);
        }
      }
    }
    load();
    return () => {
      cancelled = true;
    };
  }, [mission.id]);

  const snapshot = delivery?.snapshot;
  const noPrimary = snapshot && snapshot.primary_deliverables.length === 0;
  const caveatText = useMemo(() => snapshot?.caveats.join("\n") ?? "", [snapshot]);

  if (loading) {
    return <section className={styles.workspace}>Preparing delivery summary…</section>;
  }

  if (error) {
    return (
      <section className={styles.workspace}>
        <h2>Delivery Workspace</h2>
        <p className={styles.warning}>Delivery summary could not be loaded: {error}</p>
        <MissionChatPanel missionId={mission.id} enabled onFollowupCreated={onFollowupCreated} />
      </section>
    );
  }

  return (
    <section className={styles.workspace}>
      <header className={styles.header}>
        <div>
          <p className={styles.eyebrow}>Delivery Workspace</p>
          <h2>{snapshot?.overview.title ?? mission.title}</h2>
          <p>{snapshot?.overview.summary ?? "No delivery snapshot is available yet."}</p>
        </div>
        {delivery?.generation_status ? <span className={styles.badge}>{delivery.generation_status}</span> : null}
      </header>

      {snapshot?.overview.result ? <p className={styles.result}>{snapshot.overview.result}</p> : null}
      {noPrimary ? <p className={styles.warning}>No explicit packaged deliverable was identified. Review source output or ask follow-up chat to package/export it.</p> : null}
      {caveatText ? <pre className={styles.caveats}>{caveatText}</pre> : null}

      <div className={styles.grid}>
        <section className={styles.card}>
          <h3>Primary Delivery</h3>
          {snapshot?.primary_deliverables.length ? (
            snapshot.primary_deliverables.map((item) => (
              <article key={item.id} className={styles.deliverable}>
                <div>
                  <strong>{item.label}</strong>
                  <span>{item.kind}</span>
                </div>
                {item.path ? <code>{item.path}</code> : null}
                {item.uri ? <code>{item.uri}</code> : null}
                <p>{item.why_this_matters}</p>
                {item.how_to_use ? <p>{item.how_to_use}</p> : null}
              </article>
            ))
          ) : (
            <p>No primary delivery selected yet.</p>
          )}
        </section>

        <section className={styles.card}>
          <h3>How to use</h3>
          {snapshot?.how_to_use.length ? (
            snapshot.how_to_use.map((step) => (
              <article key={step.title}>
                <strong>{step.title}</strong>
                <ol>
                  {step.steps.map((item) => <li key={item}>{item}</li>)}
                </ol>
                {step.commands.map((command) => <code key={command}>{command}</code>)}
              </article>
            ))
          ) : (
            <p>Ask follow-up chat for run, install, or packaging instructions.</p>
          )}
        </section>

        <section className={styles.card}>
          <h3>Validation</h3>
          {snapshot?.validation.length ? (
            snapshot.validation.map((item) => (
              <article key={item.label}>
                <strong>{item.label}</strong> <span>{item.result}</span>
                {item.command ? <code>{item.command}</code> : null}
                <p>{item.evidence}</p>
              </article>
            ))
          ) : (
            <p>No validation evidence was recorded in the delivery snapshot.</p>
          )}
        </section>

        <section className={styles.card}>
          <h3>What changed</h3>
          {snapshot?.changes.length ? (
            snapshot.changes.map((change) => (
              <article key={change.label}>
                <strong>{change.label}</strong>
                <p>{change.summary}</p>
              </article>
            ))
          ) : (
            <p>No task handoff timeline is available yet.</p>
          )}
        </section>
      </div>

      <section className={styles.chatCard}>
        <h3>Ask for changes</h3>
        <MissionChatPanel missionId={mission.id} enabled onFollowupCreated={onFollowupCreated} />
      </section>
    </section>
  );
}
```

- [ ] **Step 4: Add CSS module**

Create `src/components/mission/DeliveryWorkspace.module.css`:

```css
.workspace {
  display: flex;
  flex-direction: column;
  gap: 16px;
  padding: 16px;
  border: 1px solid var(--border-color, rgba(255, 255, 255, 0.12));
  border-radius: 16px;
  background: var(--panel-bg, rgba(255, 255, 255, 0.04));
}

.header {
  display: flex;
  justify-content: space-between;
  gap: 16px;
  align-items: flex-start;
}

.eyebrow {
  margin: 0 0 4px;
  color: var(--text-muted, #8a8f98);
  text-transform: uppercase;
  letter-spacing: 0.08em;
  font-size: 12px;
}

.badge {
  padding: 4px 10px;
  border-radius: 999px;
  background: rgba(125, 211, 252, 0.14);
  color: #7dd3fc;
  font-size: 12px;
}

.result,
.warning,
.caveats {
  margin: 0;
  padding: 10px 12px;
  border-radius: 10px;
}

.result {
  background: rgba(34, 197, 94, 0.12);
}

.warning,
.caveats {
  background: rgba(251, 191, 36, 0.12);
  white-space: pre-wrap;
}

.grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(260px, 1fr));
  gap: 12px;
}

.card,
.chatCard {
  padding: 14px;
  border: 1px solid var(--border-color, rgba(255, 255, 255, 0.1));
  border-radius: 14px;
  background: rgba(0, 0, 0, 0.16);
}

.deliverable {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.deliverable > div {
  display: flex;
  justify-content: space-between;
  gap: 12px;
}

code {
  display: block;
  padding: 6px 8px;
  border-radius: 8px;
  background: rgba(0, 0, 0, 0.24);
  overflow-x: auto;
}
```

- [ ] **Step 5: Replace completed-state blocks in MissionsView**

In `src/views/MissionsView.tsx`, import:

```tsx
import { DeliveryWorkspace } from "../components/mission/DeliveryWorkspace";
```

Replace the current completed-only `MissionDeliveryPanel` block plus completed/failed `MissionChatPanel` block with:

```tsx
{selectedMission &&
(selectedMission.status === "completed" || selectedMission.status === "failed") ? (
  <DeliveryWorkspace
    mission={selectedMission}
    onFollowupCreated={(childId) => {
      selectMission(childId);
    }}
  />
) : null}
```

Keep `deliveredPayloads` temporarily if other code still uses `mission-delivered` for realtime counts. If it becomes unused after this replacement, remove the state/import in the same commit.

- [ ] **Step 6: Run frontend test**

Run:

```bash
pnpm test src/components/mission/DeliveryWorkspace.test.tsx
```

Expected: PASS.

- [ ] **Step 7: Run frontend typecheck**

Run:

```bash
pnpm tsc --noEmit
```

Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add src/components/mission/DeliveryWorkspace.tsx src/components/mission/DeliveryWorkspace.module.css src/components/mission/DeliveryWorkspace.test.tsx src/views/MissionsView.tsx src/i18n/locales/en-US.json src/i18n/locales/zh-CN.json
git commit -m "feat(delivery): add completed mission workspace" -m "Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

## Task 8: Add delivery context to follow-up chat

**Files:**
- Modify: `src-tauri/src/agent/chat.rs`
- Modify: `src-tauri/src/commands/chat.rs`

- [ ] **Step 1: Add prompt helper test**

In `src-tauri/src/agent/delivery.rs`, add:

```rust
#[test]
fn render_delivery_for_prompt_includes_primary_deliverable_and_caveat() {
    let mut snapshot = MissionDeliverySnapshot::degraded("m1", "Build app", "completed", vec![], vec![]);
    snapshot.primary_deliverables.push(DeliveryItem {
        id: "app".into(),
        kind: "macos_app".into(),
        label: "Miragenty.app".into(),
        path: Some("target/release/bundle/macos/Miragenty.app".into()),
        uri: None,
        is_primary: true,
        why_this_matters: "Runnable app".into(),
        how_to_use: Some("Open it".into()),
        evidence: vec![],
        confidence: DeliveryConfidence::High,
        warnings: vec![],
    });
    snapshot.caveats.push("Not notarized".into());

    let rendered = render_delivery_for_prompt(&snapshot, 2_000);
    assert!(rendered.contains("Miragenty.app"));
    assert!(rendered.contains("Not notarized"));
}
```

- [ ] **Step 2: Run helper test**

Run:

```bash
cd src-tauri
cargo test render_delivery_for_prompt_includes_primary_deliverable_and_caveat --lib
```

Expected: PASS if Task 2 helper exists.

- [ ] **Step 3: Inject delivery snapshot in chat system prompt**

In `src-tauri/src/agent/chat.rs`, find `build_system_prompt(...)`. After the existing published artifacts section, add a delivery snapshot section.

Use this pattern:

```rust
let delivery_context = self.db.with_conn(|conn| {
    let Some(row) = crate::db::queries::get_mission_delivery(conn, &self.mission_id)? else {
        return Ok(String::new());
    };
    let snapshot = serde_json::from_str::<crate::agent::delivery::MissionDeliverySnapshot>(&row.snapshot_json)
        .map_err(|e| anyhow::anyhow!("invalid delivery snapshot json: {e}"))?;
    Ok(crate::agent::delivery::render_delivery_for_prompt(&snapshot, 4_000))
}).unwrap_or_else(|e| {
    tracing::warn!(mission_id = %self.mission_id, error = %e, "failed to load delivery context for chat");
    String::new()
});

if !delivery_context.trim().is_empty() {
    prompt.push_str("\n\n");
    prompt.push_str(&delivery_context);
}
```

Adapt `self.mission_id` and prompt variable names to actual code.

- [ ] **Step 4: Include delivery context in follow-up proposal child mission description**

In `src-tauri/src/commands/chat.rs`, find `confirm_followup_proposal`. It already appends parent artifact context. Add delivery snapshot summary before or after artifacts:

```rust
let delivery_md = match queries::get_mission_delivery(conn, &request.parent_mission_id)? {
    Some(row) => match serde_json::from_str::<crate::agent::delivery::MissionDeliverySnapshot>(&row.snapshot_json) {
        Ok(snapshot) => format!(
            "\n\nParent delivery context:\n- Result: {}\n- Primary deliverables: {}\n- Caveats: {}\n",
            snapshot.overview.result,
            snapshot
                .primary_deliverables
                .iter()
                .map(|item| item.label.clone())
                .collect::<Vec<_>>()
                .join(", "),
            snapshot.caveats.join("; ")
        ),
        Err(_) => String::new(),
    },
    None => String::new(),
};
```

Append `delivery_md` into the child mission description string.

- [ ] **Step 5: Run Rust checks**

Run:

```bash
cd src-tauri
cargo check
cargo test render_delivery_for_prompt_includes_primary_deliverable_and_caveat --lib
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/agent/chat.rs src-tauri/src/commands/chat.rs src-tauri/src/agent/delivery.rs
git commit -m "feat(delivery): seed follow-up chat with delivery context" -m "Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

## Task 9: Add delivery data to mission reports and ReportView

**Files:**
- Modify: `src-tauri/src/agent/report_generator.rs`
- Modify: `src/ipc/commands.ts`
- Create: `src/components/report/ReportDeliverySection.tsx`
- Modify: `src/components/report/index.ts`
- Modify: `src/views/ReportView.tsx`

- [ ] **Step 1: Add report generator test first**

In `src-tauri/src/agent/report_generator.rs`, add a test near existing tests:

```rust
#[cfg(test)]
mod delivery_report_tests {
    use super::*;
    use crate::agent::delivery::{DeliveryConfidence, DeliveryItem, DeliveryOverview, DeliveryStatus, MissionDeliverySnapshot};

    #[test]
    fn markdown_includes_delivery_snapshot_section() {
        let mut report = MissionReport {
            schema_version: REPORT_SCHEMA_VERSION,
            mission: ReportMission {
                id: "m1".into(),
                title: "Build app".into(),
                description: "Create app".into(),
                status: "completed".into(),
                started_at: "2026-06-12".into(),
                completed_at: Some("2026-06-12".into()),
                duration_seconds: 1,
                total_cost_usd: 0.0,
                main_branch: None,
            },
            summary: ReportSummary { executive: "Done".into(), metrics: ReportMetrics::default() },
            decisions: vec![],
            evaluator_review: ReportEvaluatorReview { rounds: vec![], total_issues: 0, auto_fixed: 0 },
            task_matrix: vec![],
            cost_breakdown: ReportCostBreakdown::default(),
            limitations: vec![],
            contract: None,
            artifacts: vec![],
            delivery: None,
            learning_flywheel: ReportLearningFlywheel { past_decision_patterns: vec![], insight: "".into() },
        };
        report.delivery = Some(MissionDeliverySnapshot {
            mission_id: "m1".into(),
            generated_at: "2026-06-12".into(),
            status: DeliveryStatus::Completed,
            overview: DeliveryOverview { title: "Build app".into(), summary: "Packaged".into(), user_goal: "Create app".into(), result: "Delivered app".into() },
            primary_deliverables: vec![DeliveryItem { id: "app".into(), kind: "macos_app".into(), label: "App.app".into(), path: Some("dist/App.app".into()), uri: None, is_primary: true, why_this_matters: "Runnable".into(), how_to_use: Some("Open it".into()), evidence: vec![], confidence: DeliveryConfidence::High, warnings: vec![] }],
            supporting_deliverables: vec![],
            how_to_use: vec![],
            validation: vec![],
            changes: vec![],
            caveats: vec![],
            next_steps: vec![],
            report_id: None,
        });

        let markdown = render_markdown(&report);
        assert!(markdown.contains("## Delivery"));
        assert!(markdown.contains("App.app"));
        assert!(markdown.contains("dist/App.app"));
    }
}
```

If `ReportMetrics` or `ReportCostBreakdown` do not implement `Default`, add explicit field values matching their structs instead of deriving defaults.

- [ ] **Step 2: Run report test to verify it fails**

Run:

```bash
cd src-tauri
cargo test markdown_includes_delivery_snapshot_section --lib
```

Expected: FAIL because `MissionReport.delivery` does not exist.

- [ ] **Step 3: Add delivery to report model and aggregation**

In `src-tauri/src/agent/report_generator.rs`, add to `MissionReport`:

```rust
#[serde(default)]
pub delivery: Option<crate::agent::delivery::MissionDeliverySnapshot>,
```

In `aggregate_data(...)` after artifacts are collected, query delivery:

```rust
let delivery = queries::get_mission_delivery(conn, mission_id)
    .ok()
    .flatten()
    .and_then(|row| serde_json::from_str::<crate::agent::delivery::MissionDeliverySnapshot>(&row.snapshot_json).ok());
```

Include `delivery` in the returned `MissionReport`.

- [ ] **Step 4: Add Markdown rendering**

In `render_markdown(report: &MissionReport)`, add after artifacts or before limitations:

```rust
if let Some(delivery) = &report.delivery {
    out.push_str("\n## Delivery\n\n");
    out.push_str(&format!("{}\n\n", delivery.overview.result));
    if !delivery.primary_deliverables.is_empty() {
        out.push_str("### Primary Deliverables\n\n");
        for item in &delivery.primary_deliverables {
            out.push_str(&format!("- **{}** ({})", item.label, item.kind));
            if let Some(path) = &item.path {
                out.push_str(&format!(" — `{}`", path));
            }
            if let Some(uri) = &item.uri {
                out.push_str(&format!(" — {}", uri));
            }
            out.push_str(&format!("\n  - {}\n", item.why_this_matters));
        }
        out.push('\n');
    }
    if !delivery.how_to_use.is_empty() {
        out.push_str("### How to use\n\n");
        for step in &delivery.how_to_use {
            out.push_str(&format!("- **{}**\n", step.title));
            for line in &step.steps {
                out.push_str(&format!("  - {}\n", line));
            }
        }
        out.push('\n');
    }
    if !delivery.caveats.is_empty() {
        out.push_str("### Caveats\n\n");
        for caveat in &delivery.caveats {
            out.push_str(&format!("- {}\n", caveat));
        }
        out.push('\n');
    }
}
```

- [ ] **Step 5: Add frontend report delivery section**

Create `src/components/report/ReportDeliverySection.tsx`:

```tsx
import { MissionDeliverySnapshot } from "../../ipc/commands";

interface ReportDeliverySectionProps {
  delivery?: MissionDeliverySnapshot | null;
}

export function ReportDeliverySection({ delivery }: ReportDeliverySectionProps) {
  if (!delivery) {
    return <p>No delivery snapshot is attached to this report.</p>;
  }
  return (
    <div>
      <p>{delivery.overview.result}</p>
      <h4>Primary Deliverables</h4>
      {delivery.primary_deliverables.length ? (
        <ul>
          {delivery.primary_deliverables.map((item) => (
            <li key={item.id}>
              <strong>{item.label}</strong> ({item.kind})
              {item.path ? <code>{item.path}</code> : null}
              {item.uri ? <code>{item.uri}</code> : null}
              <p>{item.why_this_matters}</p>
            </li>
          ))}
        </ul>
      ) : (
        <p>No primary deliverables were identified.</p>
      )}
      {delivery.caveats.length ? (
        <>
          <h4>Caveats</h4>
          <ul>{delivery.caveats.map((caveat) => <li key={caveat}>{caveat}</li>)}</ul>
        </>
      ) : null}
    </div>
  );
}
```

Export it in `src/components/report/index.ts`:

```ts
export { ReportDeliverySection } from "./ReportDeliverySection";
```

Add `delivery?: MissionDeliverySnapshot | null;` to `MissionReportData` in `src/ipc/commands.ts`.

- [ ] **Step 6: Render delivery in ReportView**

In `src/views/ReportView.tsx`, import `ReportDeliverySection`, add a section entry named `delivery`, and render:

```tsx
<SectionWrapper id="delivery" title="Delivery" active={activeSection === "delivery"}>
  <ReportDeliverySection delivery={report.report.delivery} />
</SectionWrapper>
```

Place it before Known Limitations.

- [ ] **Step 7: Run checks**

Run:

```bash
cd src-tauri
cargo test markdown_includes_delivery_snapshot_section --lib
cargo check
cd ..
pnpm tsc --noEmit
```

Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add src-tauri/src/agent/report_generator.rs src/ipc/commands.ts src/components/report/ReportDeliverySection.tsx src/components/report/index.ts src/views/ReportView.tsx
git commit -m "feat(delivery): include delivery in reports" -m "Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

## Task 10: Add optional LLM curator and schema fallback

**Files:**
- Modify: `src-tauri/src/agent/delivery.rs`
- Modify: `src-tauri/src/commands/delivery.rs`
- Modify: `src-tauri/src/agent/scheduler.rs`

- [ ] **Step 1: Add invalid-curator-output fallback test**

In `src-tauri/src/agent/delivery.rs`, add:

```rust
#[test]
fn invalid_curator_json_falls_back_to_degraded_snapshot() {
    let fallback = MissionDeliverySnapshot::degraded("m1", "Build app", "completed", vec![], vec![]);
    let parsed = parse_curator_snapshot_or_fallback("not json", fallback.clone());
    assert_eq!(parsed.mission_id, fallback.mission_id);
    assert_eq!(parsed.status, fallback.status);
    assert!(parsed.caveats.iter().any(|c| c.contains("model curation output was invalid")));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run:

```bash
cd src-tauri
cargo test invalid_curator_json_falls_back_to_degraded_snapshot --lib
```

Expected: FAIL because helper is missing.

- [ ] **Step 3: Implement parser fallback**

Add in `src-tauri/src/agent/delivery.rs`:

```rust
pub fn parse_curator_snapshot_or_fallback(
    raw: &str,
    mut fallback: MissionDeliverySnapshot,
) -> MissionDeliverySnapshot {
    match serde_json::from_str::<MissionDeliverySnapshot>(raw) {
        Ok(snapshot) => snapshot,
        Err(err) => {
            fallback.caveats.push(format!(
                "Delivery model curation output was invalid; using fallback snapshot: {err}"
            ));
            fallback
        }
    }
}
```

- [ ] **Step 4: Add async LLM curator function**

Add a best-effort function in `src-tauri/src/agent/delivery.rs`:

```rust
pub async fn curate_delivery_with_llm(
    provider: std::sync::Arc<dyn crate::llm::LlmProvider>,
    model: &str,
    fallback: MissionDeliverySnapshot,
    candidates: &[DeliveryCandidate],
    handoffs: &[TaskHandoffPacket],
) -> MissionDeliverySnapshot {
    let prompt = format!(
        "You are Miragenty's Delivery Curator. Decide what the user actually received and how to use it. Return ONLY JSON matching MissionDeliverySnapshot.\n\nFallback snapshot:\n{}\n\nCandidates:\n{}\n\nHandoffs:\n{}",
        serde_json::to_string_pretty(&fallback).unwrap_or_default(),
        serde_json::to_string_pretty(candidates).unwrap_or_default(),
        serde_json::to_string_pretty(handoffs).unwrap_or_default(),
    );
    let request = crate::llm::LlmRequest {
        model: model.to_string(),
        messages: vec![crate::llm::Message {
            role: crate::llm::MessageRole::User,
            content: vec![crate::llm::ContentBlock::Text { text: prompt }],
        }],
        system: Some("Return strict JSON only. Do not wrap in Markdown.".into()),
        max_tokens: Some(4_000),
        temperature: Some(0.1),
        tools: Vec::new(),
        tool_choice: None,
    };
    match tokio::time::timeout(std::time::Duration::from_secs(30), provider.complete(request)).await {
        Ok(Ok(response)) => {
            let raw = response
                .content
                .into_iter()
                .filter_map(|block| match block { crate::llm::ContentBlock::Text { text } => Some(text), _ => None })
                .collect::<Vec<_>>()
                .join("\n");
            parse_curator_snapshot_or_fallback(&raw, fallback)
        }
        Ok(Err(err)) => {
            let mut snapshot = fallback;
            snapshot.caveats.push(format!("Delivery model curation failed; using fallback snapshot: {err}"));
            snapshot
        }
        Err(_) => {
            let mut snapshot = fallback;
            snapshot.caveats.push("Delivery model curation timed out; using fallback snapshot.".into());
            snapshot
        }
    }
}
```

Adapt `LlmRequest` fields to the actual struct in `src-tauri/src/llm` if names differ.

- [ ] **Step 5: Use curator in explicit generate command**

In `src-tauri/src/commands/delivery.rs`, update `generate_mission_delivery` to be async and build provider via `commands::mission::build_provider(&app)`:

```rust
#[tauri::command]
pub async fn generate_mission_delivery(
    app: tauri::AppHandle,
    mission_id: String,
) -> Result<GenerateMissionDeliveryResponse, String> {
    let db = app.state::<Database>();
    let fallback = db.with_conn(|conn| crate::agent::delivery::generate_degraded_delivery_snapshot(conn, &mission_id))
        .map_err(|e| e.to_string())?;

    let (snapshot, status, model_name) = match crate::commands::mission::build_provider(&app).ok() {
        Some((provider, model)) => {
            let snapshot = crate::agent::delivery::curate_delivery_with_llm(provider, &model, fallback, &[], &[]).await;
            (snapshot, "generated", Some(model))
        }
        None => (fallback, "degraded", Some("deterministic-fallback".into())),
    };

    db.with_conn(|conn| crate::agent::delivery::persist_delivery_snapshot(conn, &snapshot, status, model_name.as_deref()))
        .map_err(|e| e.to_string())?;

    Ok(GenerateMissionDeliveryResponse { mission_id, generation_status: status.into() })
}
```

Keep scheduler auto-generation deterministic for now to avoid long background LLM calls during mission terminal handling. The UI can call explicit generation when needed.

- [ ] **Step 6: Run checks**

Run:

```bash
cd src-tauri
cargo test invalid_curator_json_falls_back_to_degraded_snapshot --lib
cargo check
```

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/agent/delivery.rs src-tauri/src/commands/delivery.rs src-tauri/src/agent/scheduler.rs
git commit -m "feat(delivery): add model-curated snapshots" -m "Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

## Task 11: Full verification and polish

**Files:**
- Potential fixes from verification only.

- [ ] **Step 1: Run backend tests**

Run:

```bash
cd src-tauri
cargo test --lib
```

Expected: PASS. If failures occur, fix the smallest related issue and rerun the failing test first, then rerun `cargo test --lib`.

- [ ] **Step 2: Run backend compile check**

Run:

```bash
cd src-tauri
cargo check
```

Expected: PASS.

- [ ] **Step 3: Run frontend tests**

Run:

```bash
pnpm test
```

Expected: PASS.

- [ ] **Step 4: Run frontend typecheck**

Run:

```bash
pnpm tsc --noEmit
```

Expected: PASS.

- [ ] **Step 5: Run lint**

Run:

```bash
pnpm lint
```

Expected: PASS. If lint reports existing unrelated warnings, record them clearly and fix only delivery-related warnings.

- [ ] **Step 6: Manual app verification**

Run the app in development mode:

```bash
pnpm tauri dev
```

Manual checks:

1. Open an existing completed mission.
2. Confirm the right-side/main mission area shows Delivery Workspace, not only DAG details.
3. Confirm `Preparing delivery summary…` resolves to snapshot content.
4. Confirm if no delivery exists, the UI calls generation and then reloads.
5. Confirm follow-up chat is visible in the workspace.
6. Generate/open a mission report and confirm the Delivery section appears.

- [ ] **Step 7: Review git diff for accidental unrelated changes**

Run:

```bash
git status --short
git diff --stat
```

Expected: only Mission Delivery Plane files changed. Do not include unrelated old ripgrep/deepseek worktree artifacts unless they are already intentionally part of the branch.

- [ ] **Step 8: Final commit if verification fixes were needed**

If Step 1-7 required fixes:

```bash
git add <fixed-files>
git commit -m "fix(delivery): polish delivery workspace verification" -m "Co-Authored-By: Claude <noreply@anthropic.com>"
```

If no fixes were needed, skip this commit.

---

## Self-Review Checklist

- Spec coverage:
  - Durable `task_handoff_packets`: Task 1 and Task 3.
  - Durable `mission_deliveries`: Task 1 and Task 5.
  - Candidate discovery as hints, not hard rules: Task 2 and Task 5.
  - Model-curated snapshot with fallback: Task 10.
  - Downstream prompt injection: Task 4.
  - Completed Delivery Workspace UI: Task 7.
  - Follow-up chat context: Task 8.
  - Report integration: Task 9.
  - Error/degraded states: Task 2, Task 5, Task 7, Task 10.
  - Verification: Task 11.
- Placeholder scan: no implementation step uses unresolved placeholder language. Where code must adapt to existing names, the plan gives exact fallback instructions.
- Type consistency:
  - Rust structs use snake_case serde output.
  - Frontend types mirror backend JSON names.
  - `MissionDeliverySnapshot`, `DeliveryItem`, and status enum values are used consistently across backend, IPC, UI, and report.
