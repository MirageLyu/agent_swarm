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
}
