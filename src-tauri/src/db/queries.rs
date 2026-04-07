/// Reusable query helpers for agent_events, agents, and scheduler tables.
use anyhow::Result;
use rusqlite::{params, Connection};

pub fn insert_agent(conn: &Connection, id: &str, name: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO agents (id, name, status) VALUES (?1, ?2, 'idle')",
        params![id, name],
    )?;
    Ok(())
}

pub fn insert_event(
    conn: &Connection,
    id: &str,
    agent_id: &str,
    step: i64,
    kind: &str,
    content: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO agent_events (id, agent_id, step, kind, content) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![id, agent_id, step, kind, content],
    )?;
    Ok(())
}

pub struct EventRow {
    pub id: String,
    pub agent_id: String,
    pub step: i64,
    pub kind: String,
    pub content: String,
    pub created_at: String,
}

pub fn get_events_for_agent(conn: &Connection, agent_id: &str) -> Result<Vec<EventRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, agent_id, step, kind, content, created_at
         FROM agent_events WHERE agent_id = ?1 ORDER BY created_at ASC",
    )?;
    let rows = stmt
        .query_map(params![agent_id], |row| {
            Ok(EventRow {
                id: row.get(0)?,
                agent_id: row.get(1)?,
                step: row.get(2)?,
                kind: row.get(3)?,
                content: row.get(4)?,
                created_at: row.get(5)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

// ---- Scheduler query helpers ----

pub struct ReadyTask {
    pub id: String,
    pub title: String,
    pub description: String,
}

pub fn get_ready_tasks_for_mission(
    conn: &Connection,
    mission_id: &str,
    limit: i64,
) -> Result<Vec<ReadyTask>> {
    let mut stmt = conn.prepare(
        "SELECT id, title, description FROM tasks
         WHERE mission_id = ?1 AND status = 'ready'
         LIMIT ?2",
    )?;
    let tasks = stmt
        .query_map(params![mission_id, limit], |row| {
            Ok(ReadyTask {
                id: row.get(0)?,
                title: row.get(1)?,
                description: row.get(2)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(tasks)
}

/// Atomically claim a task: set ready → running. Returns true if this call won the claim.
pub fn claim_task(conn: &Connection, task_id: &str) -> Result<bool> {
    let rows = conn.execute(
        "UPDATE tasks SET status = 'running' WHERE id = ?1 AND status = 'ready'",
        [task_id],
    )?;
    Ok(rows > 0)
}

pub fn complete_task(conn: &Connection, task_id: &str) -> Result<()> {
    conn.execute(
        "UPDATE tasks SET status = 'completed', completed_at = datetime('now') WHERE id = ?1",
        [task_id],
    )?;
    Ok(())
}

pub fn fail_task(conn: &Connection, task_id: &str, status: &str) -> Result<()> {
    conn.execute(
        "UPDATE tasks SET status = ?1 WHERE id = ?2",
        params![status, task_id],
    )?;
    Ok(())
}

/// After a task completes, promote any downstream tasks whose deps are now fully met.
/// Returns IDs of tasks promoted from pending → ready.
pub fn advance_dependencies(conn: &Connection, completed_task_id: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT task_id FROM task_dependencies WHERE depends_on = ?1",
    )?;
    let downstream: Vec<String> = stmt
        .query_map([completed_task_id], |row| row.get(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    let mut promoted = Vec::new();
    for downstream_id in downstream {
        let unmet: i64 = conn.query_row(
            "SELECT COUNT(*) FROM task_dependencies td
             JOIN tasks t ON t.id = td.depends_on
             WHERE td.task_id = ?1 AND t.status != 'completed'",
            [&downstream_id],
            |row| row.get(0),
        )?;

        if unmet == 0 {
            let rows = conn.execute(
                "UPDATE tasks SET status = 'ready' WHERE id = ?1 AND status = 'pending'",
                [&downstream_id],
            )?;
            if rows > 0 {
                promoted.push(downstream_id);
            }
        }
    }
    Ok(promoted)
}

/// Check if a mission has reached terminal state.
/// Returns Some("completed") or Some("failed") if terminal, None otherwise.
/// Side effect: updates missions.status when terminal.
pub fn check_mission_terminal(conn: &Connection, mission_id: &str) -> Result<Option<String>> {
    let mission_status: String = conn.query_row(
        "SELECT status FROM missions WHERE id = ?1",
        [mission_id],
        |row| row.get(0),
    )?;

    if mission_status == "completed" || mission_status == "failed" {
        return Ok(Some(mission_status));
    }

    let total: i64 = conn.query_row(
        "SELECT COUNT(*) FROM tasks WHERE mission_id = ?1",
        [mission_id],
        |row| row.get(0),
    )?;
    if total == 0 {
        return Ok(None);
    }

    let completed: i64 = conn.query_row(
        "SELECT COUNT(*) FROM tasks WHERE mission_id = ?1 AND status = 'completed'",
        [mission_id],
        |row| row.get(0),
    )?;
    if completed == total {
        conn.execute(
            "UPDATE missions SET status = 'completed', updated_at = datetime('now') WHERE id = ?1 AND status != 'completed'",
            [mission_id],
        )?;
        return Ok(Some("completed".to_string()));
    }

    let running: i64 = conn.query_row(
        "SELECT COUNT(*) FROM tasks WHERE mission_id = ?1 AND status = 'running'",
        [mission_id],
        |row| row.get(0),
    )?;
    let ready: i64 = conn.query_row(
        "SELECT COUNT(*) FROM tasks WHERE mission_id = ?1 AND status = 'ready'",
        [mission_id],
        |row| row.get(0),
    )?;
    let failed_or_cancelled: i64 = conn.query_row(
        "SELECT COUNT(*) FROM tasks WHERE mission_id = ?1 AND status IN ('failed', 'cancelled')",
        [mission_id],
        |row| row.get(0),
    )?;

    if failed_or_cancelled > 0 && running == 0 && ready == 0 {
        conn.execute(
            "UPDATE missions SET status = 'failed', updated_at = datetime('now') WHERE id = ?1 AND status != 'failed'",
            [mission_id],
        )?;
        return Ok(Some("failed".to_string()));
    }

    Ok(None)
}

pub fn count_running_agents(conn: &Connection) -> Result<i64> {
    let count = conn.query_row(
        "SELECT COUNT(*) FROM agents WHERE status = 'running'",
        [],
        |row| row.get(0),
    )?;
    Ok(count)
}

pub fn insert_agent_for_task(
    conn: &Connection,
    agent_id: &str,
    name: &str,
    task_id: &str,
    worktree_path: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO agents (id, name, task_id, status, worktree_path) VALUES (?1, ?2, ?3, 'idle', ?4)",
        params![agent_id, name, task_id, worktree_path],
    )?;
    conn.execute(
        "UPDATE tasks SET assigned_agent_id = ?1 WHERE id = ?2",
        params![agent_id, task_id],
    )?;
    Ok(())
}

/// Returns agent IDs for completed tasks in a mission, ordered by DAG topology
/// (dependencies first, then by completion time).
pub fn get_completed_agents_topo_order(
    conn: &Connection,
    mission_id: &str,
) -> Result<Vec<String>> {
    // Kahn's algorithm: tasks with fewer unmet deps come first, ties broken by completed_at.
    // Since all tasks are completed, we sort by depth-in-DAG then completed_at.
    let mut stmt = conn.prepare(
        "WITH RECURSIVE topo AS (
            -- Seed: tasks with no dependencies (depth 0)
            SELECT t.id, t.assigned_agent_id, t.completed_at, 0 AS depth
            FROM tasks t
            WHERE t.mission_id = ?1 AND t.status = 'completed'
              AND NOT EXISTS (SELECT 1 FROM task_dependencies td WHERE td.task_id = t.id)

            UNION ALL

            -- Recursive: tasks whose all deps are already in topo
            SELECT t.id, t.assigned_agent_id, t.completed_at, topo.depth + 1
            FROM tasks t
            JOIN task_dependencies td ON td.task_id = t.id
            JOIN topo ON topo.id = td.depends_on
            WHERE t.mission_id = ?1 AND t.status = 'completed'
        )
        SELECT DISTINCT assigned_agent_id
        FROM topo
        WHERE assigned_agent_id IS NOT NULL
        ORDER BY depth ASC, completed_at ASC",
    )?;

    let agents: Vec<String> = stmt
        .query_map(params![mission_id], |row| row.get(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    Ok(agents)
}

// ---- FM-04: Activity stream & cost tracking ----

pub struct CostSummary {
    pub total_cost: f64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
}

pub fn get_mission_cost_summary(conn: &Connection, mission_id: &str) -> Result<CostSummary> {
    let summary = conn.query_row(
        "SELECT COALESCE(SUM(cr.cost_usd), 0.0),
                COALESCE(SUM(cr.input_tokens), 0),
                COALESCE(SUM(cr.output_tokens), 0)
         FROM cost_records cr
         JOIN agents a ON a.id = cr.agent_id
         JOIN tasks t ON t.id = a.task_id
         WHERE t.mission_id = ?1",
        params![mission_id],
        |row| {
            Ok(CostSummary {
                total_cost: row.get(0)?,
                total_input_tokens: row.get(1)?,
                total_output_tokens: row.get(2)?,
            })
        },
    )?;
    Ok(summary)
}

pub fn list_agent_events(
    conn: &Connection,
    mission_id: Option<&str>,
    agent_id: Option<&str>,
) -> Result<Vec<EventRow>> {
    match (mission_id, agent_id) {
        (_, Some(aid)) => {
            get_events_for_agent(conn, aid)
        }
        (Some(mid), None) => {
            let mut stmt = conn.prepare(
                "SELECT ae.id, ae.agent_id, ae.step, ae.kind, ae.content, ae.created_at
                 FROM agent_events ae
                 JOIN agents a ON a.id = ae.agent_id
                 JOIN tasks t ON t.id = a.task_id
                 WHERE t.mission_id = ?1
                 ORDER BY ae.created_at ASC",
            )?;
            let rows = stmt
                .query_map(params![mid], |row| {
                    Ok(EventRow {
                        id: row.get(0)?,
                        agent_id: row.get(1)?,
                        step: row.get(2)?,
                        kind: row.get(3)?,
                        content: row.get(4)?,
                        created_at: row.get(5)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        }
        (None, None) => {
            let mut stmt = conn.prepare(
                "SELECT id, agent_id, step, kind, content, created_at
                 FROM agent_events ORDER BY created_at ASC LIMIT 500",
            )?;
            let rows = stmt
                .query_map([], |row| {
                    Ok(EventRow {
                        id: row.get(0)?,
                        agent_id: row.get(1)?,
                        step: row.get(2)?,
                        kind: row.get(3)?,
                        content: row.get(4)?,
                        created_at: row.get(5)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        }
    }
}

pub fn save_agent_base_commit(conn: &Connection, agent_id: &str, hash: &str) -> Result<()> {
    conn.execute(
        "UPDATE agents SET base_commit_hash = ?1 WHERE id = ?2",
        params![hash, agent_id],
    )?;
    Ok(())
}

pub fn save_agent_head_commit(conn: &Connection, agent_id: &str, hash: &str) -> Result<()> {
    conn.execute(
        "UPDATE agents SET head_commit_hash = ?1 WHERE id = ?2",
        params![hash, agent_id],
    )?;
    Ok(())
}

pub struct AgentCommitHashes {
    pub base_commit_hash: Option<String>,
    pub head_commit_hash: Option<String>,
}

pub fn get_agent_commit_hashes(conn: &Connection, agent_id: &str) -> Result<AgentCommitHashes> {
    conn.query_row(
        "SELECT base_commit_hash, head_commit_hash FROM agents WHERE id = ?1",
        params![agent_id],
        |row| {
            Ok(AgentCommitHashes {
                base_commit_hash: row.get(0)?,
                head_commit_hash: row.get(1)?,
            })
        },
    )
    .map_err(|e| anyhow::anyhow!("Agent not found: {e}"))
}

/// Returns the latest review action for an agent, or None if never reviewed.
pub fn get_latest_review_status(conn: &Connection, agent_id: &str) -> Result<Option<String>> {
    let result: Option<String> = conn
        .query_row(
            "SELECT content FROM agent_events
             WHERE agent_id = ?1 AND kind = 'review'
             ORDER BY created_at DESC LIMIT 1",
            params![agent_id],
            |row| row.get(0),
        )
        .ok();

    match result {
        Some(content) => {
            let v: serde_json::Value = serde_json::from_str(&content)
                .unwrap_or(serde_json::Value::Null);
            Ok(v.get("action").and_then(|a| a.as_str()).map(String::from))
        }
        None => Ok(None),
    }
}

// ---- FM-06: Agent notes (runtime intervention) ----

pub struct NoteRow {
    pub id: String,
    pub agent_id: String,
    pub content: String,
    pub status: String,
    pub created_at: String,
    pub applied_at: Option<String>,
    pub mission_id: Option<String>,
}

pub fn insert_note(conn: &Connection, id: &str, agent_id: &str, content: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO agent_notes (id, agent_id, content) VALUES (?1, ?2, ?3)",
        params![id, agent_id, content],
    )?;
    Ok(())
}

pub fn insert_note_for_mission(
    conn: &Connection,
    id: &str,
    agent_id: &str,
    mission_id: &str,
    content: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO agent_notes (id, agent_id, mission_id, content) VALUES (?1, ?2, ?3, ?4)",
        params![id, agent_id, mission_id, content],
    )?;
    Ok(())
}

pub fn get_running_agent_ids_for_mission(
    conn: &Connection,
    mission_id: &str,
) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT a.id FROM agents a
         JOIN tasks t ON a.task_id = t.id
         WHERE t.mission_id = ?1 AND a.status = 'running'",
    )?;
    let ids = stmt
        .query_map(params![mission_id], |row| row.get(0))?
        .collect::<std::result::Result<Vec<String>, _>>()?;
    Ok(ids)
}

pub fn list_notes_for_mission(conn: &Connection, mission_id: &str) -> Result<Vec<NoteRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, agent_id, content, status, created_at, applied_at, mission_id
         FROM agent_notes
         WHERE mission_id = ?1
         ORDER BY created_at DESC
         LIMIT 20",
    )?;
    let rows = stmt
        .query_map(params![mission_id], |row| {
            Ok(NoteRow {
                id: row.get(0)?,
                agent_id: row.get(1)?,
                content: row.get(2)?,
                status: row.get(3)?,
                created_at: row.get(4)?,
                applied_at: row.get(5)?,
                mission_id: row.get(6)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn poll_queued_notes(conn: &Connection, agent_id: &str) -> Result<Vec<NoteRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, agent_id, content, status, created_at, applied_at, mission_id
         FROM agent_notes
         WHERE agent_id = ?1 AND status = 'queued'
         ORDER BY created_at ASC",
    )?;
    let rows = stmt
        .query_map(params![agent_id], |row| {
            Ok(NoteRow {
                id: row.get(0)?,
                agent_id: row.get(1)?,
                content: row.get(2)?,
                status: row.get(3)?,
                created_at: row.get(4)?,
                applied_at: row.get(5)?,
                mission_id: row.get(6)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn mark_notes_applied(conn: &Connection, note_ids: &[String]) -> Result<()> {
    for id in note_ids {
        conn.execute(
            "UPDATE agent_notes SET status = 'applied', applied_at = datetime('now') WHERE id = ?1",
            params![id],
        )?;
    }
    Ok(())
}

pub fn expire_notes_for_agent(conn: &Connection, agent_id: &str) -> Result<u64> {
    let rows = conn.execute(
        "UPDATE agent_notes SET status = 'expired' WHERE agent_id = ?1 AND status = 'queued'",
        params![agent_id],
    )?;
    Ok(rows as u64)
}

pub fn list_notes_for_agent(conn: &Connection, agent_id: &str) -> Result<Vec<NoteRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, agent_id, content, status, created_at, applied_at, mission_id
         FROM agent_notes
         WHERE agent_id = ?1
         ORDER BY created_at DESC
         LIMIT 10",
    )?;
    let rows = stmt
        .query_map(params![agent_id], |row| {
            Ok(NoteRow {
                id: row.get(0)?,
                agent_id: row.get(1)?,
                content: row.get(2)?,
                status: row.get(3)?,
                created_at: row.get(4)?,
                applied_at: row.get(5)?,
                mission_id: row.get(6)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

// ---- FM-06: Mission directives (persistent notes for future agents) ----

pub fn append_mission_directive(conn: &Connection, mission_id: &str, directive: &str) -> Result<()> {
    conn.execute(
        "UPDATE missions SET directives = CASE
            WHEN directives = '' THEN ?1
            ELSE directives || char(10) || ?1
         END,
         updated_at = datetime('now')
         WHERE id = ?2",
        params![directive, mission_id],
    )?;
    Ok(())
}

pub fn get_mission_directives(conn: &Connection, mission_id: &str) -> Result<String> {
    let directives: String = conn.query_row(
        "SELECT directives FROM missions WHERE id = ?1",
        params![mission_id],
        |row| row.get(0),
    )?;
    Ok(directives)
}

pub fn get_mission_id_for_agent(conn: &Connection, agent_id: &str) -> Result<Option<String>> {
    let result: Option<String> = conn
        .query_row(
            "SELECT t.mission_id FROM agents a
             JOIN tasks t ON a.task_id = t.id
             WHERE a.id = ?1",
            params![agent_id],
            |row| row.get(0),
        )
        .ok();
    Ok(result)
}

// ---- FM-08: Mission lifecycle queries ----

pub fn get_mission_repo_path(conn: &Connection, mission_id: &str) -> Result<Option<String>> {
    let path: Option<String> = conn.query_row(
        "SELECT repo_path FROM missions WHERE id = ?1",
        params![mission_id],
        |row| row.get(0),
    )?;
    Ok(path)
}

pub fn delete_agents_for_mission(conn: &Connection, mission_id: &str) -> Result<u64> {
    let rows = conn.execute(
        "DELETE FROM agents WHERE task_id IN (SELECT id FROM tasks WHERE mission_id = ?1)",
        params![mission_id],
    )?;
    Ok(rows as u64)
}

pub fn delete_agents_for_tasks(conn: &Connection, task_ids: &[String]) -> Result<u64> {
    if task_ids.is_empty() {
        return Ok(0);
    }
    let placeholders = task_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!("DELETE FROM agents WHERE task_id IN ({placeholders})");
    let param_refs: Vec<&dyn rusqlite::types::ToSql> =
        task_ids.iter().map(|id| id as &dyn rusqlite::types::ToSql).collect();
    let rows = conn.execute(&sql, param_refs.as_slice())?;
    Ok(rows as u64)
}

/// Reset all tasks in a mission to initial state.
/// Tasks with no dependencies → ready, tasks with dependencies → pending.
/// Returns the number of tasks reset.
pub fn reset_all_tasks(conn: &Connection, mission_id: &str) -> Result<u32> {
    // Reset all tasks to pending first
    let rows = conn.execute(
        "UPDATE tasks SET status = 'pending', assigned_agent_id = NULL, completed_at = NULL
         WHERE mission_id = ?1",
        params![mission_id],
    )? as u32;

    // Promote tasks with no dependencies to ready
    conn.execute(
        "UPDATE tasks SET status = 'ready'
         WHERE mission_id = ?1 AND status = 'pending'
           AND id NOT IN (SELECT task_id FROM task_dependencies)",
        params![mission_id],
    )?;

    Ok(rows)
}

/// Reset only failed/cancelled tasks in a mission.
/// Returns the number of tasks reset.
pub fn reset_failed_tasks(conn: &Connection, mission_id: &str) -> Result<u32> {
    // Get failed/cancelled task ids
    let mut stmt = conn.prepare(
        "SELECT id FROM tasks WHERE mission_id = ?1 AND status IN ('failed', 'cancelled')",
    )?;
    let failed_ids: Vec<String> = stmt
        .query_map(params![mission_id], |row| row.get(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    if failed_ids.is_empty() {
        return Ok(0);
    }

    let count = failed_ids.len() as u32;

    // Reset these tasks
    let placeholders = failed_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        "UPDATE tasks SET status = 'pending', assigned_agent_id = NULL, completed_at = NULL
         WHERE id IN ({placeholders})"
    );
    let param_refs: Vec<&dyn rusqlite::types::ToSql> =
        failed_ids.iter().map(|id| id as &dyn rusqlite::types::ToSql).collect();
    conn.execute(&sql, param_refs.as_slice())?;

    // Promote to ready if all upstream deps are completed
    for tid in &failed_ids {
        let unmet: i64 = conn.query_row(
            "SELECT COUNT(*) FROM task_dependencies td
             JOIN tasks t ON t.id = td.depends_on
             WHERE td.task_id = ?1 AND t.status != 'completed'",
            params![tid],
            |row| row.get(0),
        )?;

        if unmet == 0 {
            conn.execute(
                "UPDATE tasks SET status = 'ready' WHERE id = ?1 AND status = 'pending'",
                params![tid],
            )?;
        }
    }

    Ok(count)
}

pub fn count_failed_tasks(conn: &Connection, mission_id: &str) -> Result<u32> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM tasks WHERE mission_id = ?1 AND status IN ('failed', 'cancelled')",
        params![mission_id],
        |row| row.get(0),
    )?;
    Ok(count as u32)
}

pub fn reset_orphaned_running_tasks(conn: &Connection, mission_id: &str) -> Result<u64> {
    let rows = conn.execute(
        "UPDATE tasks SET status = 'ready', assigned_agent_id = NULL
         WHERE mission_id = ?1 AND status = 'running'",
        [mission_id],
    )?;
    Ok(rows as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrations_run_on;
    use rusqlite::Connection;

    fn setup_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        migrations_run_on(&conn).unwrap();
        conn
    }

    #[test]
    fn insert_and_query_llm_call_event() {
        let conn = setup_db();
        insert_agent(&conn, "agent-1", "Test Agent").unwrap();
        insert_event(&conn, "evt-1", "agent-1", 1, "llm_call", "Step 1: calling LLM").unwrap();

        let events = get_events_for_agent(&conn, "agent-1").unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "llm_call");
        assert_eq!(events[0].step, 1);
    }

    #[test]
    fn insert_tool_result_event() {
        let conn = setup_db();
        insert_agent(&conn, "agent-1", "Test Agent").unwrap();
        insert_event(&conn, "evt-1", "agent-1", 2, "tool_result", "file contents here").unwrap();

        let events = get_events_for_agent(&conn, "agent-1").unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "tool_result");
        assert_eq!(events[0].content, "file contents here");
    }

    #[test]
    fn insert_error_event() {
        let conn = setup_db();
        insert_agent(&conn, "agent-1", "Test Agent").unwrap();
        insert_event(
            &conn,
            "evt-1",
            "agent-1",
            3,
            "error",
            r#"{"error":"shell_error","message":"exit code 1"}"#,
        )
        .unwrap();

        let events = get_events_for_agent(&conn, "agent-1").unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "error");
        assert!(events[0].content.contains("shell_error"));
    }

    #[test]
    fn multiple_events_ordered() {
        let conn = setup_db();
        insert_agent(&conn, "agent-1", "Test Agent").unwrap();
        insert_event(&conn, "evt-1", "agent-1", 1, "llm_call", "step 1").unwrap();
        insert_event(&conn, "evt-2", "agent-1", 1, "tool_use", "tool call").unwrap();
        insert_event(&conn, "evt-3", "agent-1", 1, "tool_result", "tool out").unwrap();
        insert_event(&conn, "evt-4", "agent-1", 1, "checkpoint", "tokens: 100in/50out").unwrap();

        let events = get_events_for_agent(&conn, "agent-1").unwrap();
        assert_eq!(events.len(), 4);
        let kinds: Vec<&str> = events.iter().map(|e| e.kind.as_str()).collect();
        assert_eq!(kinds, vec!["llm_call", "tool_use", "tool_result", "checkpoint"]);
    }

    #[test]
    fn status_change_event_accepted() {
        let conn = setup_db();
        insert_agent(&conn, "agent-1", "Test Agent").unwrap();
        insert_event(&conn, "evt-1", "agent-1", 0, "status_change", "running").unwrap();
        insert_event(&conn, "evt-2", "agent-1", 5, "status_change", "cancelled").unwrap();

        let events = get_events_for_agent(&conn, "agent-1").unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].kind, "status_change");
        assert_eq!(events[1].content, "cancelled");
    }

    #[test]
    fn events_isolated_per_agent() {
        let conn = setup_db();
        insert_agent(&conn, "agent-1", "Agent 1").unwrap();
        insert_agent(&conn, "agent-2", "Agent 2").unwrap();
        insert_event(&conn, "evt-1", "agent-1", 1, "llm_call", "a1 step").unwrap();
        insert_event(&conn, "evt-2", "agent-2", 1, "llm_call", "a2 step").unwrap();

        let events1 = get_events_for_agent(&conn, "agent-1").unwrap();
        let events2 = get_events_for_agent(&conn, "agent-2").unwrap();
        assert_eq!(events1.len(), 1);
        assert_eq!(events2.len(), 1);
        assert_eq!(events1[0].content, "a1 step");
        assert_eq!(events2[0].content, "a2 step");
    }

    // ---- FM-02 Scheduler Tests ----

    fn create_mission(conn: &Connection, id: &str) {
        conn.execute(
            "INSERT INTO missions (id, title, status) VALUES (?1, 'Test Mission', 'running')",
            [id],
        )
        .unwrap();
    }

    fn create_task(conn: &Connection, id: &str, mission_id: &str, status: &str) {
        conn.execute(
            "INSERT INTO tasks (id, mission_id, title, description, complexity, status) VALUES (?1, ?2, ?3, 'desc', 'medium', ?4)",
            rusqlite::params![id, mission_id, format!("Task {id}"), status],
        )
        .unwrap();
    }

    fn add_dep(conn: &Connection, task_id: &str, depends_on: &str) {
        conn.execute(
            "INSERT INTO task_dependencies (task_id, depends_on) VALUES (?1, ?2)",
            [task_id, depends_on],
        )
        .unwrap();
    }

    fn task_status(conn: &Connection, id: &str) -> String {
        conn.query_row("SELECT status FROM tasks WHERE id = ?1", [id], |row| {
            row.get(0)
        })
        .unwrap()
    }

    fn mission_status(conn: &Connection, id: &str) -> String {
        conn.query_row("SELECT status FROM missions WHERE id = ?1", [id], |row| {
            row.get(0)
        })
        .unwrap()
    }

    // UT-01: Scheduler task selection

    #[test]
    fn ut01_1_select_single_ready_task() {
        let conn = setup_db();
        create_mission(&conn, "m1");
        create_task(&conn, "t1", "m1", "ready");
        let tasks = get_ready_tasks_for_mission(&conn, "m1", 10).unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, "t1");
    }

    #[test]
    fn ut01_2_respect_concurrency_limit() {
        let conn = setup_db();
        create_mission(&conn, "m1");
        create_task(&conn, "t1", "m1", "ready");
        create_task(&conn, "t2", "m1", "ready");
        create_task(&conn, "t3", "m1", "ready");
        let tasks = get_ready_tasks_for_mission(&conn, "m1", 2).unwrap();
        assert_eq!(tasks.len(), 2);
    }

    #[test]
    fn ut01_3_no_ready_tasks() {
        let conn = setup_db();
        create_mission(&conn, "m1");
        create_task(&conn, "t1", "m1", "pending");
        let tasks = get_ready_tasks_for_mission(&conn, "m1", 10).unwrap();
        assert_eq!(tasks.len(), 0);
    }

    #[test]
    fn ut01_4_atomic_claim() {
        let conn = setup_db();
        create_mission(&conn, "m1");
        create_task(&conn, "t1", "m1", "ready");

        assert!(claim_task(&conn, "t1").unwrap());
        assert_eq!(task_status(&conn, "t1"), "running");
        assert!(!claim_task(&conn, "t1").unwrap());
    }

    // UT-02: Dependency advancement

    #[test]
    fn ut02_1_single_dep_satisfied() {
        let conn = setup_db();
        create_mission(&conn, "m1");
        create_task(&conn, "t1", "m1", "completed");
        create_task(&conn, "t2", "m1", "pending");
        add_dep(&conn, "t2", "t1");

        let promoted = advance_dependencies(&conn, "t1").unwrap();
        assert_eq!(promoted, vec!["t2"]);
        assert_eq!(task_status(&conn, "t2"), "ready");
    }

    #[test]
    fn ut02_2_multi_dep_partial() {
        let conn = setup_db();
        create_mission(&conn, "m1");
        create_task(&conn, "t1", "m1", "completed");
        create_task(&conn, "t2", "m1", "running");
        create_task(&conn, "t3", "m1", "pending");
        add_dep(&conn, "t3", "t1");
        add_dep(&conn, "t3", "t2");

        let promoted = advance_dependencies(&conn, "t1").unwrap();
        assert!(promoted.is_empty());
        assert_eq!(task_status(&conn, "t3"), "pending");
    }

    #[test]
    fn ut02_3_multi_dep_all_satisfied() {
        let conn = setup_db();
        create_mission(&conn, "m1");
        create_task(&conn, "t1", "m1", "completed");
        create_task(&conn, "t2", "m1", "completed");
        create_task(&conn, "t3", "m1", "pending");
        add_dep(&conn, "t3", "t1");
        add_dep(&conn, "t3", "t2");

        let promoted = advance_dependencies(&conn, "t2").unwrap();
        assert_eq!(promoted, vec!["t3"]);
        assert_eq!(task_status(&conn, "t3"), "ready");
    }

    #[test]
    fn ut02_4_upstream_failed() {
        let conn = setup_db();
        create_mission(&conn, "m1");
        create_task(&conn, "t1", "m1", "failed");
        create_task(&conn, "t2", "m1", "pending");
        add_dep(&conn, "t2", "t1");

        let promoted = advance_dependencies(&conn, "t1").unwrap();
        assert!(promoted.is_empty());
        assert_eq!(task_status(&conn, "t2"), "pending");
    }

    // UT-04: Mission terminal state

    #[test]
    fn ut04_1_all_completed() {
        let conn = setup_db();
        create_mission(&conn, "m1");
        create_task(&conn, "t1", "m1", "completed");
        create_task(&conn, "t2", "m1", "completed");

        let result = check_mission_terminal(&conn, "m1").unwrap();
        assert_eq!(result, Some("completed".to_string()));
        assert_eq!(mission_status(&conn, "m1"), "completed");
    }

    #[test]
    fn ut04_2_some_failed_no_running() {
        let conn = setup_db();
        create_mission(&conn, "m1");
        create_task(&conn, "t1", "m1", "completed");
        create_task(&conn, "t2", "m1", "failed");

        let result = check_mission_terminal(&conn, "m1").unwrap();
        assert_eq!(result, Some("failed".to_string()));
        assert_eq!(mission_status(&conn, "m1"), "failed");
    }

    #[test]
    fn ut04_3_still_running() {
        let conn = setup_db();
        create_mission(&conn, "m1");
        create_task(&conn, "t1", "m1", "running");
        create_task(&conn, "t2", "m1", "pending");

        let result = check_mission_terminal(&conn, "m1").unwrap();
        assert_eq!(result, None);
        assert_eq!(mission_status(&conn, "m1"), "running");
    }

    // ---- FM-04: Cost summary tests (UT-01) ----

    fn insert_cost_record(
        conn: &Connection,
        id: &str,
        agent_id: &str,
        task_id: &str,
        input_tokens: i64,
        output_tokens: i64,
        cost: f64,
    ) {
        conn.execute(
            "INSERT INTO cost_records (id, agent_id, task_id, model, input_tokens, output_tokens, cost_usd)
             VALUES (?1, ?2, ?3, 'test-model', ?4, ?5, ?6)",
            rusqlite::params![id, agent_id, task_id, input_tokens, output_tokens, cost],
        )
        .unwrap();
    }

    #[test]
    fn ut01_1_single_agent_single_step() {
        let conn = setup_db();
        create_mission(&conn, "m1");
        create_task(&conn, "t1", "m1", "running");
        insert_agent_for_task(&conn, "a1", "Agent 1", "t1", "/tmp/wt").unwrap();
        insert_cost_record(&conn, "cr1", "a1", "t1", 100, 50, 0.0015);

        let summary = get_mission_cost_summary(&conn, "m1").unwrap();
        assert!((summary.total_cost - 0.0015).abs() < 1e-6);
        assert_eq!(summary.total_input_tokens, 100);
        assert_eq!(summary.total_output_tokens, 50);
    }

    #[test]
    fn ut01_2_multi_agent_summary() {
        let conn = setup_db();
        create_mission(&conn, "m1");
        create_task(&conn, "t1", "m1", "completed");
        create_task(&conn, "t2", "m1", "completed");
        create_task(&conn, "t3", "m1", "completed");
        insert_agent_for_task(&conn, "a1", "Agent 1", "t1", "/tmp/w1").unwrap();
        insert_agent_for_task(&conn, "a2", "Agent 2", "t2", "/tmp/w2").unwrap();
        insert_agent_for_task(&conn, "a3", "Agent 3", "t3", "/tmp/w3").unwrap();
        insert_cost_record(&conn, "cr1", "a1", "t1", 100, 50, 0.001);
        insert_cost_record(&conn, "cr2", "a1", "t1", 200, 80, 0.002);
        insert_cost_record(&conn, "cr3", "a2", "t2", 150, 60, 0.0015);
        insert_cost_record(&conn, "cr4", "a3", "t3", 300, 100, 0.003);

        let summary = get_mission_cost_summary(&conn, "m1").unwrap();
        assert!((summary.total_cost - 0.0075).abs() < 1e-6);
        assert_eq!(summary.total_input_tokens, 750);
        assert_eq!(summary.total_output_tokens, 290);
    }

    #[test]
    fn ut01_3_empty_records() {
        let conn = setup_db();
        create_mission(&conn, "m1");

        let summary = get_mission_cost_summary(&conn, "m1").unwrap();
        assert!((summary.total_cost).abs() < 1e-6);
        assert_eq!(summary.total_input_tokens, 0);
        assert_eq!(summary.total_output_tokens, 0);
    }

    // ---- FM-04: list_agent_events tests ----

    #[test]
    fn list_events_by_agent_id() {
        let conn = setup_db();
        create_mission(&conn, "m1");
        create_task(&conn, "t1", "m1", "running");
        insert_agent_for_task(&conn, "a1", "Agent 1", "t1", "/tmp/w1").unwrap();
        insert_event(&conn, "e1", "a1", 1, "llm_call", "step 1").unwrap();
        insert_event(&conn, "e2", "a1", 2, "tool_use", "step 2").unwrap();

        let events = list_agent_events(&conn, None, Some("a1")).unwrap();
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn list_events_by_mission_id() {
        let conn = setup_db();
        create_mission(&conn, "m1");
        create_task(&conn, "t1", "m1", "running");
        create_task(&conn, "t2", "m1", "running");
        insert_agent_for_task(&conn, "a1", "Agent 1", "t1", "/tmp/w1").unwrap();
        insert_agent_for_task(&conn, "a2", "Agent 2", "t2", "/tmp/w2").unwrap();
        insert_event(&conn, "e1", "a1", 1, "llm_call", "a1 s1").unwrap();
        insert_event(&conn, "e2", "a2", 1, "llm_call", "a2 s1").unwrap();

        let events = list_agent_events(&conn, Some("m1"), None).unwrap();
        assert_eq!(events.len(), 2);
    }

    // ---- FM-05: Review status tests ----

    #[test]
    fn review_status_none_by_default() {
        let conn = setup_db();
        insert_agent(&conn, "agent-1", "Test Agent").unwrap();
        let status = get_latest_review_status(&conn, "agent-1").unwrap();
        assert!(status.is_none());
    }

    #[test]
    fn review_status_approved() {
        let conn = setup_db();
        insert_agent(&conn, "agent-1", "Test Agent").unwrap();
        insert_event(
            &conn,
            "rev-1",
            "agent-1",
            0,
            "review",
            r#"{"action":"approved","comment":""}"#,
        )
        .unwrap();
        let status = get_latest_review_status(&conn, "agent-1").unwrap();
        assert_eq!(status.as_deref(), Some("approved"));
    }

    #[test]
    fn review_status_latest_wins() {
        let conn = setup_db();
        insert_agent(&conn, "agent-1", "Test Agent").unwrap();
        insert_event(
            &conn,
            "rev-1",
            "agent-1",
            0,
            "review",
            r#"{"action":"approved","comment":""}"#,
        )
        .unwrap();
        insert_event(
            &conn,
            "rev-2",
            "agent-1",
            0,
            "review",
            r#"{"action":"revision_requested","comment":"fix tests"}"#,
        )
        .unwrap();
        let status = get_latest_review_status(&conn, "agent-1").unwrap();
        assert_eq!(status.as_deref(), Some("revision_requested"));
    }

    #[test]
    fn ut04_4_all_cancelled() {
        let conn = setup_db();
        create_mission(&conn, "m1");
        create_task(&conn, "t1", "m1", "cancelled");
        create_task(&conn, "t2", "m1", "cancelled");

        let result = check_mission_terminal(&conn, "m1").unwrap();
        assert_eq!(result, Some("failed".to_string()));
        assert_eq!(mission_status(&conn, "m1"), "failed");
    }

    // ---- FM-06: Agent notes tests ----

    #[test]
    fn ut06_01_1_insert_note_queued() {
        let conn = setup_db();
        insert_agent(&conn, "a1", "Agent 1").unwrap();
        insert_note(&conn, "n1", "a1", "Focus on error handling").unwrap();

        let notes = poll_queued_notes(&conn, "a1").unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].id, "n1");
        assert_eq!(notes[0].status, "queued");
        assert_eq!(notes[0].content, "Focus on error handling");
    }

    #[test]
    fn ut06_01_2_mark_applied() {
        let conn = setup_db();
        insert_agent(&conn, "a1", "Agent 1").unwrap();
        insert_note(&conn, "n1", "a1", "note 1").unwrap();
        insert_note(&conn, "n2", "a1", "note 2").unwrap();

        mark_notes_applied(&conn, &["n1".to_string()]).unwrap();

        let queued = poll_queued_notes(&conn, "a1").unwrap();
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].id, "n2");

        let all = list_notes_for_agent(&conn, "a1").unwrap();
        let applied = all.iter().find(|n| n.id == "n1").unwrap();
        assert_eq!(applied.status, "applied");
        assert!(applied.applied_at.is_some());
    }

    #[test]
    fn ut06_01_3_expire_notes() {
        let conn = setup_db();
        insert_agent(&conn, "a1", "Agent 1").unwrap();
        insert_note(&conn, "n1", "a1", "note 1").unwrap();
        insert_note(&conn, "n2", "a1", "note 2").unwrap();

        let expired = expire_notes_for_agent(&conn, "a1").unwrap();
        assert_eq!(expired, 2);

        let queued = poll_queued_notes(&conn, "a1").unwrap();
        assert!(queued.is_empty());

        let all = list_notes_for_agent(&conn, "a1").unwrap();
        assert!(all.iter().all(|n| n.status == "expired"));
    }

    #[test]
    fn ut06_02_1_notes_ordered_by_created_at() {
        let conn = setup_db();
        insert_agent(&conn, "a1", "Agent 1").unwrap();
        insert_note(&conn, "n1", "a1", "first").unwrap();
        insert_note(&conn, "n2", "a1", "second").unwrap();
        insert_note(&conn, "n3", "a1", "third").unwrap();

        let notes = poll_queued_notes(&conn, "a1").unwrap();
        assert_eq!(notes.len(), 3);
        assert_eq!(notes[0].content, "first");
        assert_eq!(notes[1].content, "second");
        assert_eq!(notes[2].content, "third");
    }

    #[test]
    fn ut06_02_2_list_capped_at_10() {
        let conn = setup_db();
        insert_agent(&conn, "a1", "Agent 1").unwrap();
        for i in 0..15 {
            insert_note(&conn, &format!("n{i}"), "a1", &format!("note {i}")).unwrap();
        }

        let listed = list_notes_for_agent(&conn, "a1").unwrap();
        assert_eq!(listed.len(), 10);
    }

    #[test]
    fn ut06_expire_only_queued_not_applied() {
        let conn = setup_db();
        insert_agent(&conn, "a1", "Agent 1").unwrap();
        insert_note(&conn, "n1", "a1", "applied note").unwrap();
        insert_note(&conn, "n2", "a1", "queued note").unwrap();

        mark_notes_applied(&conn, &["n1".to_string()]).unwrap();
        let expired = expire_notes_for_agent(&conn, "a1").unwrap();
        assert_eq!(expired, 1);

        let all = list_notes_for_agent(&conn, "a1").unwrap();
        let n1 = all.iter().find(|n| n.id == "n1").unwrap();
        let n2 = all.iter().find(|n| n.id == "n2").unwrap();
        assert_eq!(n1.status, "applied");
        assert_eq!(n2.status, "expired");
    }

    #[test]
    fn ut06_notes_isolated_per_agent() {
        let conn = setup_db();
        insert_agent(&conn, "a1", "Agent 1").unwrap();
        insert_agent(&conn, "a2", "Agent 2").unwrap();
        insert_note(&conn, "n1", "a1", "for agent 1").unwrap();
        insert_note(&conn, "n2", "a2", "for agent 2").unwrap();

        let notes1 = poll_queued_notes(&conn, "a1").unwrap();
        let notes2 = poll_queued_notes(&conn, "a2").unwrap();
        assert_eq!(notes1.len(), 1);
        assert_eq!(notes2.len(), 1);
        assert_eq!(notes1[0].content, "for agent 1");
        assert_eq!(notes2[0].content, "for agent 2");
    }

    #[test]
    fn ut06_mission_note_fan_out() {
        let conn = setup_db();
        create_mission(&conn, "m1");
        create_task(&conn, "t1", "m1", "running");
        create_task(&conn, "t2", "m1", "running");
        insert_agent_for_task(&conn, "a1", "Agent 1", "t1", "/tmp/w1").unwrap();
        insert_agent_for_task(&conn, "a2", "Agent 2", "t2", "/tmp/w2").unwrap();
        conn.execute("UPDATE agents SET status = 'running' WHERE id IN ('a1', 'a2')", []).unwrap();

        let running = get_running_agent_ids_for_mission(&conn, "m1").unwrap();
        assert_eq!(running.len(), 2);

        for (i, aid) in running.iter().enumerate() {
            insert_note_for_mission(&conn, &format!("n{i}"), aid, "m1", "use strict mode").unwrap();
        }

        let a1_notes = poll_queued_notes(&conn, "a1").unwrap();
        let a2_notes = poll_queued_notes(&conn, "a2").unwrap();
        assert_eq!(a1_notes.len(), 1);
        assert_eq!(a2_notes.len(), 1);
        assert_eq!(a1_notes[0].content, "use strict mode");
        assert_eq!(a1_notes[0].mission_id.as_deref(), Some("m1"));

        let mission_notes = list_notes_for_mission(&conn, "m1").unwrap();
        assert_eq!(mission_notes.len(), 2);
    }

    #[test]
    fn ut06_mission_note_skips_completed_agents() {
        let conn = setup_db();
        create_mission(&conn, "m1");
        create_task(&conn, "t1", "m1", "running");
        create_task(&conn, "t2", "m1", "completed");
        insert_agent_for_task(&conn, "a1", "Agent 1", "t1", "/tmp/w1").unwrap();
        insert_agent_for_task(&conn, "a2", "Agent 2", "t2", "/tmp/w2").unwrap();
        conn.execute("UPDATE agents SET status = 'running' WHERE id = 'a1'", []).unwrap();
        conn.execute("UPDATE agents SET status = 'completed' WHERE id = 'a2'", []).unwrap();

        let running = get_running_agent_ids_for_mission(&conn, "m1").unwrap();
        assert_eq!(running.len(), 1);
        assert_eq!(running[0], "a1");
    }
}
