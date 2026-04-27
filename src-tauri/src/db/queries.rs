/// Reusable query helpers for agent_events, agents, and scheduler tables.
use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};

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

/// 全局口径：当前所有 mission 下处于 `running` 状态的 agent 总数（监控 / dashboard 用）。
pub fn count_running_agents(conn: &Connection) -> Result<i64> {
    let count = conn.query_row(
        "SELECT COUNT(*) FROM agents WHERE status = 'running'",
        [],
        |row| row.get(0),
    )?;
    Ok(count)
}

/// FM-15 Phase 2 (FR-13): 按 mission 隔离的 running agent 计数。
/// Scheduler 用这个口径计算每个 mission 自己的并发槽位，
/// 避免多个 mission 共享同一份 `max_concurrent_agents` 配额。
pub fn count_running_agents_for_mission(
    conn: &Connection,
    mission_id: &str,
) -> Result<i64> {
    let count = conn.query_row(
        "SELECT COUNT(*) FROM agents a
         JOIN tasks t ON t.id = a.task_id
         WHERE a.status = 'running' AND t.mission_id = ?1",
        params![mission_id],
        |row| row.get(0),
    )?;
    Ok(count)
}

/// FM-15 Phase 2 (FR-12): 读取 mission 已缓存的主分支名（NULL 表示未探测）。
pub fn get_mission_main_branch(
    conn: &Connection,
    mission_id: &str,
) -> Result<Option<String>> {
    let value: Option<String> = conn
        .query_row(
            "SELECT main_branch FROM missions WHERE id = ?1",
            params![mission_id],
            |row| row.get::<_, Option<String>>(0),
        )?;
    Ok(value)
}

/// FM-15 Phase 2 (FR-12): 把探测到的主分支名缓存回 mission 行。
pub fn set_mission_main_branch(
    conn: &Connection,
    mission_id: &str,
    main_branch: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE missions SET main_branch = ?1, updated_at = datetime('now') WHERE id = ?2",
        params![main_branch, mission_id],
    )?;
    Ok(())
}

/// FM-15 Phase 2 (FR-07.6): 读取 mission 是否启用增量 worktree（默认 true）。
pub fn get_mission_use_incremental_worktree(
    conn: &Connection,
    mission_id: &str,
) -> Result<bool> {
    let v: i64 = conn
        .query_row(
            "SELECT COALESCE(use_incremental_worktree, 1) FROM missions WHERE id = ?1",
            params![mission_id],
            |row| row.get(0),
        )?;
    Ok(v != 0)
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

/// FM-15 Phase 2 (FR-07): 单个 task 的已完成直接父任务 + 其 agent 信息，
/// 按拓扑深度升序（即父辈 task 间也维持 DAG 顺序），同深度按 completed_at 升序。
///
/// 返回 `(parent_task_id, parent_agent_id)`。`parent_agent_id` 缺失（理论上不该发生）
/// 的行会被过滤，因为 `prepare_task_base` 需要从 `agent/<id>` 分支取内容。
pub fn get_completed_parent_tasks_for(
    conn: &Connection,
    task_id: &str,
) -> Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare(
        "WITH RECURSIVE ancestors AS (
            SELECT t.id, t.assigned_agent_id, t.completed_at, 0 AS depth
            FROM task_dependencies td
            JOIN tasks t ON t.id = td.depends_on
            WHERE td.task_id = ?1 AND t.status = 'completed'

            UNION ALL

            SELECT t.id, t.assigned_agent_id, t.completed_at, ancestors.depth + 1
            FROM task_dependencies td
            JOIN tasks t ON t.id = td.depends_on
            JOIN ancestors ON ancestors.id = td.task_id
            WHERE t.status = 'completed'
        )
        SELECT DISTINCT id, assigned_agent_id, depth, completed_at
        FROM ancestors
        WHERE id IN (
            SELECT depends_on FROM task_dependencies WHERE task_id = ?1
        )
        ORDER BY depth ASC, completed_at ASC",
    )?;

    let rows: Vec<(String, Option<String>)> = stmt
        .query_map(params![task_id], |row| {
            let id: String = row.get(0)?;
            let agent_id: Option<String> = row.get(1)?;
            Ok((id, agent_id))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    Ok(rows
        .into_iter()
        .filter_map(|(t, a)| a.map(|a| (t, a)))
        .collect())
}

/// FM-15 Phase 2 (FR-08.1): mission 内"frontier"已完成任务列表 —— 即"自身已完成
/// 且没有任何已完成 successor"的任务。配合增量 worktree（FR-07）的语义：
/// frontier 任务的 commit 已经累计包含了它所有上游的产物，因此 mission 终态
/// 合并时只需 merge frontier，不必再逐 agent 拓扑序合并。
///
/// 返回 `(task_id, agent_id, completed_at)`。`agent_id` 缺失的 task 会被过滤。
pub fn get_frontier_completed_tasks(
    conn: &Connection,
    mission_id: &str,
) -> Result<Vec<(String, String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT t.id, t.assigned_agent_id, COALESCE(t.completed_at, datetime('now'))
         FROM tasks t
         WHERE t.mission_id = ?1
           AND t.status = 'completed'
           AND NOT EXISTS (
             SELECT 1 FROM task_dependencies td
             JOIN tasks ts ON ts.id = td.task_id
             WHERE td.depends_on = t.id AND ts.status = 'completed'
           )
         ORDER BY t.completed_at ASC, t.id ASC",
    )?;

    let rows = stmt
        .query_map(params![mission_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    Ok(rows
        .into_iter()
        .filter_map(|(t, a, c)| a.map(|a| (t, a, c)))
        .collect())
}

/// FM-15 Phase 2 (FR-08.3): 写入一条 merge 记录到 `merge_records`。
/// `conflicted_files` 通常是 JSON-serialized `Vec<String>`，由调用方负责序列化。
#[allow(clippy::too_many_arguments)]
pub fn record_merge_attempt(
    conn: &Connection,
    id: &str,
    mission_id: &str,
    source_branch: &str,
    target_branch: &str,
    strategy_attempted: &str,
    final_strategy: &str,
    conflicted_files_json: &str,
    llm_resolution_succeeded: Option<bool>,
    build_passed_after_llm: Option<bool>,
    fallback_reason: Option<&str>,
) -> Result<()> {
    conn.execute(
        "INSERT INTO merge_records
         (id, mission_id, source_branch, target_branch,
          strategy_attempted, final_strategy, conflicted_files,
          llm_resolution_succeeded, build_passed_after_llm, fallback_reason)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            id,
            mission_id,
            source_branch,
            target_branch,
            strategy_attempted,
            final_strategy,
            conflicted_files_json,
            llm_resolution_succeeded.map(|b| if b { 1i64 } else { 0i64 }),
            build_passed_after_llm.map(|b| if b { 1i64 } else { 0i64 }),
            fallback_reason,
        ],
    )?;
    Ok(())
}

/// FM-15 Phase 2 (FR-08.3): 读取某 mission 的全部 merge 记录，按时间升序。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MergeRecord {
    pub id: String,
    pub mission_id: String,
    pub source_branch: String,
    pub target_branch: String,
    pub strategy_attempted: String,
    pub final_strategy: String,
    pub conflicted_files_json: String,
    pub llm_resolution_succeeded: Option<bool>,
    pub build_passed_after_llm: Option<bool>,
    pub fallback_reason: Option<String>,
    pub created_at: String,
}

pub fn get_merge_records_for_mission(
    conn: &Connection,
    mission_id: &str,
) -> Result<Vec<MergeRecord>> {
    let mut stmt = conn.prepare(
        "SELECT id, mission_id, source_branch, target_branch,
                strategy_attempted, final_strategy, conflicted_files,
                llm_resolution_succeeded, build_passed_after_llm,
                fallback_reason, created_at
         FROM merge_records
         WHERE mission_id = ?1
         ORDER BY created_at ASC, id ASC",
    )?;

    let rows = stmt
        .query_map(params![mission_id], |row| {
            Ok(MergeRecord {
                id: row.get(0)?,
                mission_id: row.get(1)?,
                source_branch: row.get(2)?,
                target_branch: row.get(3)?,
                strategy_attempted: row.get(4)?,
                final_strategy: row.get(5)?,
                conflicted_files_json: row.get(6)?,
                llm_resolution_succeeded: row.get::<_, Option<i64>>(7)?.map(|v| v != 0),
                build_passed_after_llm: row.get::<_, Option<i64>>(8)?.map(|v| v != 0),
                fallback_reason: row.get(9)?,
                created_at: row.get(10)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    Ok(rows)
}

/// FM-15 Phase 2 (FR-07.1): 把 base 构建过程中识别到的冲突写入 `task_base_conflicts`。
/// `resolution` 取自 `MergeLayer::as_resolution_str()`。
/// 用 INSERT OR REPLACE，重试场景下覆盖旧记录。
pub fn record_task_base_conflict(
    conn: &Connection,
    task_id: &str,
    parent_task_id: &str,
    file_path: &str,
    resolution: &str,
) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO task_base_conflicts
         (task_id, parent_task_id, file_path, resolution)
         VALUES (?1, ?2, ?3, ?4)",
        params![task_id, parent_task_id, file_path, resolution],
    )?;
    Ok(())
}

/// FM-15 Phase 2: 读取某 task 的所有 base 冲突记录（按 parent_task_id, file_path 排序）。
pub fn get_task_base_conflicts(
    conn: &Connection,
    task_id: &str,
) -> Result<Vec<(String, String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT parent_task_id, file_path, resolution
         FROM task_base_conflicts
         WHERE task_id = ?1
         ORDER BY parent_task_id, file_path",
    )?;

    let rows = stmt
        .query_map(params![task_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    Ok(rows)
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

// ---- FM-11: Evaluator reviews & annotations ----

pub fn insert_evaluator_review(
    conn: &Connection,
    id: &str,
    agent_id: &str,
    mission_id: &str,
    overall_score: f64,
    summary: &str,
    contract_compliance: Option<&str>,
) -> Result<()> {
    conn.execute(
        "INSERT INTO evaluator_reviews (id, agent_id, mission_id, overall_score, summary, contract_compliance)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![id, agent_id, mission_id, overall_score, summary, contract_compliance],
    )?;
    Ok(())
}

pub fn insert_evaluator_annotation(
    conn: &Connection,
    id: &str,
    review_id: &str,
    agent_id: &str,
    file_path: &str,
    line_number: i64,
    ann_type: &str,
    severity: &str,
    message: &str,
    suggestion: Option<&str>,
    auto_fixable: bool,
    original_code: Option<&str>,
    fixed_code: Option<&str>,
) -> Result<()> {
    conn.execute(
        "INSERT INTO evaluator_annotations
         (id, review_id, agent_id, file_path, line_number, type, severity, message,
          suggestion, auto_fixable, original_code, fixed_code)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            id, review_id, agent_id, file_path, line_number,
            ann_type, severity, message, suggestion,
            auto_fixable as i32, original_code, fixed_code
        ],
    )?;
    Ok(())
}

pub fn update_annotation_status(conn: &Connection, annotation_id: &str, status: &str) -> Result<bool> {
    let rows = conn.execute(
        "UPDATE evaluator_annotations SET status = ?1 WHERE id = ?2",
        params![status, annotation_id],
    )?;
    Ok(rows > 0)
}

pub struct EvaluatorReviewRow {
    pub id: String,
    pub agent_id: String,
    pub mission_id: String,
    pub overall_score: f64,
    pub summary: String,
    pub contract_compliance: Option<String>,
    pub created_at: String,
}

pub fn get_evaluator_review_for_agent(
    conn: &Connection,
    agent_id: &str,
) -> Result<Option<EvaluatorReviewRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, agent_id, mission_id, overall_score, summary, contract_compliance, created_at
         FROM evaluator_reviews WHERE agent_id = ?1
         ORDER BY created_at DESC LIMIT 1",
    )?;
    let row = stmt
        .query_row(params![agent_id], |row| {
            Ok(EvaluatorReviewRow {
                id: row.get(0)?,
                agent_id: row.get(1)?,
                mission_id: row.get(2)?,
                overall_score: row.get(3)?,
                summary: row.get(4)?,
                contract_compliance: row.get(5)?,
                created_at: row.get(6)?,
            })
        })
        .ok();
    Ok(row)
}

pub struct AnnotationRow {
    pub id: String,
    pub review_id: String,
    pub agent_id: String,
    pub file_path: String,
    pub line_number: i64,
    pub ann_type: String,
    pub severity: String,
    pub status: String,
    pub message: String,
    pub suggestion: Option<String>,
    pub auto_fixable: bool,
    pub original_code: Option<String>,
    pub fixed_code: Option<String>,
    pub created_at: String,
}

pub fn get_annotations_for_agent(
    conn: &Connection,
    agent_id: &str,
    file_path: Option<&str>,
) -> Result<Vec<AnnotationRow>> {
    let (sql, param_values): (&str, Vec<Box<dyn rusqlite::types::ToSql>>) = if let Some(fp) = file_path {
        (
            "SELECT id, review_id, agent_id, file_path, line_number, type, severity, status,
                    message, suggestion, auto_fixable, original_code, fixed_code, created_at
             FROM evaluator_annotations
             WHERE agent_id = ?1 AND file_path = ?2
             ORDER BY file_path, line_number",
            vec![Box::new(agent_id.to_string()), Box::new(fp.to_string())],
        )
    } else {
        (
            "SELECT id, review_id, agent_id, file_path, line_number, type, severity, status,
                    message, suggestion, auto_fixable, original_code, fixed_code, created_at
             FROM evaluator_annotations
             WHERE agent_id = ?1
             ORDER BY file_path, line_number",
            vec![Box::new(agent_id.to_string())],
        )
    };
    let mut stmt = conn.prepare(sql)?;
    let param_refs: Vec<&dyn rusqlite::types::ToSql> = param_values.iter().map(|b| b.as_ref()).collect();
    let rows = stmt
        .query_map(param_refs.as_slice(), |row| {
            Ok(AnnotationRow {
                id: row.get(0)?,
                review_id: row.get(1)?,
                agent_id: row.get(2)?,
                file_path: row.get(3)?,
                line_number: row.get(4)?,
                ann_type: row.get(5)?,
                severity: row.get(6)?,
                status: row.get(7)?,
                message: row.get(8)?,
                suggestion: row.get(9)?,
                auto_fixable: row.get::<_, i32>(10)? != 0,
                original_code: row.get(11)?,
                fixed_code: row.get(12)?,
                created_at: row.get(13)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn has_evaluator_review(conn: &Connection, agent_id: &str) -> Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM evaluator_reviews WHERE agent_id = ?1",
        params![agent_id],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

pub fn get_contract_quality_threshold(
    conn: &Connection,
    mission_id: &str,
) -> Result<Option<f64>> {
    let result: Option<f64> = conn
        .query_row(
            "SELECT quality_threshold FROM mission_contracts
             WHERE mission_id = ?1 AND status = 'signed'",
            params![mission_id],
            |row| row.get(0),
        )
        .ok();
    Ok(result)
}

pub fn get_task_id_for_agent(conn: &Connection, agent_id: &str) -> Result<Option<String>> {
    let result: Option<String> = conn
        .query_row(
            "SELECT task_id FROM agents WHERE id = ?1",
            params![agent_id],
            |row| row.get(0),
        )
        .ok();
    Ok(result)
}

pub fn mark_task_needs_revision(conn: &Connection, task_id: &str) -> Result<()> {
    conn.execute(
        "UPDATE tasks SET status = 'failed' WHERE id = ?1",
        params![task_id],
    )?;
    Ok(())
}

// =====================================================================
// FM-15 v2.2 Slice 1: planner_sessions / planner_steps helpers
// =====================================================================

pub fn create_planner_session(
    conn: &Connection,
    id: &str,
    mission_id: Option<&str>,
    kind: &str,
    repo_path: &str,
    description: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO planner_sessions (id, mission_id, kind, repo_path, description)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![id, mission_id, kind, repo_path, description],
    )?;
    Ok(())
}

pub fn complete_planner_session(
    conn: &Connection,
    id: &str,
    total_steps: i64,
    total_tokens: i64,
) -> Result<()> {
    conn.execute(
        "UPDATE planner_sessions
         SET status = 'completed',
             total_steps = ?2,
             total_tokens = ?3,
             completed_at = datetime('now')
         WHERE id = ?1",
        params![id, total_steps, total_tokens],
    )?;
    Ok(())
}

pub fn fail_planner_session(
    conn: &Connection,
    id: &str,
    total_steps: i64,
    total_tokens: i64,
    error_message: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE planner_sessions
         SET status = 'failed',
             total_steps = ?2,
             total_tokens = ?3,
             error_message = ?4,
             completed_at = datetime('now')
         WHERE id = ?1",
        params![id, total_steps, total_tokens, error_message],
    )?;
    Ok(())
}

pub fn link_planner_session_to_mission(
    conn: &Connection,
    session_id: &str,
    mission_id: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE planner_sessions SET mission_id = ?2 WHERE id = ?1",
        params![session_id, mission_id],
    )?;
    Ok(())
}

pub fn insert_planner_step(
    conn: &Connection,
    id: &str,
    session_id: &str,
    step_no: i64,
    kind: &str,
    tool_name: Option<&str>,
    tool_args: Option<&str>,
    tool_result: Option<&str>,
    text_content: Option<&str>,
    tokens_used: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO planner_steps
            (id, session_id, step_no, kind, tool_name, tool_args, tool_result, text_content, tokens_used)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            id,
            session_id,
            step_no,
            kind,
            tool_name,
            tool_args,
            tool_result,
            text_content,
            tokens_used,
        ],
    )?;
    Ok(())
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PlannerStepRow {
    pub id: String,
    pub session_id: String,
    pub step_no: i64,
    pub kind: String,
    pub tool_name: Option<String>,
    pub tool_args: Option<String>,
    pub tool_result: Option<String>,
    pub text_content: Option<String>,
    pub tokens_used: i64,
    pub created_at: String,
}

pub fn list_planner_steps(conn: &Connection, session_id: &str) -> Result<Vec<PlannerStepRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, session_id, step_no, kind, tool_name, tool_args, tool_result, text_content, tokens_used, created_at
         FROM planner_steps
         WHERE session_id = ?1
         ORDER BY step_no ASC",
    )?;
    let rows = stmt
        .query_map([session_id], |row| {
            Ok(PlannerStepRow {
                id: row.get(0)?,
                session_id: row.get(1)?,
                step_no: row.get(2)?,
                kind: row.get(3)?,
                tool_name: row.get(4)?,
                tool_args: row.get(5)?,
                tool_result: row.get(6)?,
                text_content: row.get(7)?,
                tokens_used: row.get(8)?,
                created_at: row.get(9)?,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();
    Ok(rows)
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PlannerSessionRow {
    pub id: String,
    pub mission_id: Option<String>,
    pub kind: String,
    pub contract_id: Option<String>,
    pub repo_path: String,
    pub description: String,
    pub status: String,
    pub total_steps: i64,
    pub total_tokens: i64,
    pub error_message: Option<String>,
    pub created_at: String,
    pub completed_at: Option<String>,
}

pub fn get_planner_session(conn: &Connection, id: &str) -> Result<Option<PlannerSessionRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, mission_id, kind, contract_id, repo_path, description, status,
                total_steps, total_tokens, error_message, created_at, completed_at
         FROM planner_sessions WHERE id = ?1",
    )?;
    let row = stmt
        .query_row([id], |row| {
            Ok(PlannerSessionRow {
                id: row.get(0)?,
                mission_id: row.get(1)?,
                kind: row.get(2)?,
                contract_id: row.get(3)?,
                repo_path: row.get(4)?,
                description: row.get(5)?,
                status: row.get(6)?,
                total_steps: row.get(7)?,
                total_tokens: row.get(8)?,
                error_message: row.get(9)?,
                created_at: row.get(10)?,
                completed_at: row.get(11)?,
            })
        })
        .ok();
    Ok(row)
}

// ---- FM-15 FR-03: Artifact 查询 ----

#[derive(Debug, Clone, serde::Serialize)]
pub struct ArtifactRow {
    pub id: String,
    pub mission_id: String,
    pub producer_task_id: String,
    #[serde(rename = "type")]
    pub artifact_type: String,
    pub local_name: String,
    pub summary: String,
    /// JSON 数组的字符串形式，调用方按需 parse
    pub file_paths: String,
    pub published: bool,
    pub created_at: String,
}

fn map_artifact_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ArtifactRow> {
    Ok(ArtifactRow {
        id: row.get(0)?,
        mission_id: row.get(1)?,
        producer_task_id: row.get(2)?,
        artifact_type: row.get(3)?,
        local_name: row.get(4)?,
        summary: row.get(5)?,
        file_paths: row.get(6)?,
        published: row.get::<_, i64>(7)? != 0,
        created_at: row.get(8)?,
    })
}

pub fn list_artifacts_for_mission(
    conn: &Connection,
    mission_id: &str,
) -> Result<Vec<ArtifactRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, mission_id, producer_task_id, type, local_name, summary, file_paths, published, created_at
         FROM artifacts WHERE mission_id = ?1 ORDER BY created_at ASC",
    )?;
    let rows = stmt
        .query_map([mission_id], map_artifact_row)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

// ---- FM-15 FR-05.x: Planner fetch_url 持久化 (grant + 计数) ----

/// 写入"该 session 下同 host 一直允许"的授权（FetchDecision::AllowSession 触发）。
/// 主键是 (session_id, host)，重复写入是 no-op。
pub fn record_planner_fetch_grant(
    conn: &Connection,
    session_id: &str,
    host: &str,
) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO planner_session_fetch_grants (session_id, domain) VALUES (?1, ?2)",
        params![session_id, host],
    )?;
    Ok(())
}

pub fn is_planner_fetch_granted(
    conn: &Connection,
    session_id: &str,
    host: &str,
) -> Result<bool> {
    let cnt: i64 = conn.query_row(
        "SELECT COUNT(*) FROM planner_session_fetch_grants
         WHERE session_id = ?1 AND domain = ?2",
        params![session_id, host],
        |row| row.get(0),
    )?;
    Ok(cnt > 0)
}

/// session 内已发起的 fetch_url 调用数（含失败/拒绝），用于配额计数。
pub fn count_planner_fetch_calls(conn: &Connection, session_id: &str) -> Result<i64> {
    let cnt: i64 = conn.query_row(
        "SELECT COUNT(*) FROM planner_steps
         WHERE session_id = ?1 AND kind = 'tool_call' AND tool_name = 'fetch_url'",
        params![session_id],
        |row| row.get(0),
    )?;
    Ok(cnt)
}

pub fn list_artifacts_for_task(conn: &Connection, task_id: &str) -> Result<Vec<ArtifactRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, mission_id, producer_task_id, type, local_name, summary, file_paths, published, created_at
         FROM artifacts WHERE producer_task_id = ?1 ORDER BY created_at ASC",
    )?;
    let rows = stmt
        .query_map([task_id], map_artifact_row)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

// ---- FM-15 v2.2 P4-S5: mission_chats / parent_mission_id ----

#[derive(Debug, Clone, serde::Serialize)]
pub struct MissionChatRow {
    pub id: String,
    pub mission_id: String,
    pub role: String,
    pub content: String,
    pub tool_calls: Option<String>,
    pub artifact_refs: Option<String>,
    pub proposed_followup_mission_id: Option<String>,
    pub created_at: String,
}

fn map_chat_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<MissionChatRow> {
    Ok(MissionChatRow {
        id: row.get(0)?,
        mission_id: row.get(1)?,
        role: row.get(2)?,
        content: row.get(3)?,
        tool_calls: row.get(4)?,
        artifact_refs: row.get(5)?,
        proposed_followup_mission_id: row.get(6)?,
        created_at: row.get(7)?,
    })
}

pub fn insert_mission_chat(
    conn: &Connection,
    id: &str,
    mission_id: &str,
    role: &str,
    content: &str,
    tool_calls: Option<&str>,
    artifact_refs: Option<&str>,
    proposed_followup_mission_id: Option<&str>,
) -> Result<()> {
    conn.execute(
        "INSERT INTO mission_chats
            (id, mission_id, role, content, tool_calls, artifact_refs, proposed_followup_mission_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            id,
            mission_id,
            role,
            content,
            tool_calls,
            artifact_refs,
            proposed_followup_mission_id,
        ],
    )?;
    Ok(())
}

pub fn list_mission_chats(
    conn: &Connection,
    mission_id: &str,
) -> Result<Vec<MissionChatRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, mission_id, role, content, tool_calls, artifact_refs,
                proposed_followup_mission_id, created_at
         FROM mission_chats
         WHERE mission_id = ?1
         ORDER BY created_at ASC, id ASC",
    )?;
    let rows = stmt
        .query_map([mission_id], map_chat_row)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// FR-15.4: 子 mission 通过 parent_mission_id 与父 mission 关联。
pub fn set_mission_parent(
    conn: &Connection,
    child_mission_id: &str,
    parent_mission_id: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE missions SET parent_mission_id = ?2 WHERE id = ?1",
        params![child_mission_id, parent_mission_id],
    )?;
    Ok(())
}

pub fn get_mission_parent(conn: &Connection, mission_id: &str) -> Result<Option<String>> {
    let row = conn
        .query_row(
            "SELECT parent_mission_id FROM missions WHERE id = ?1",
            [mission_id],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()?;
    Ok(row.flatten())
}

pub fn list_followup_mission_ids(
    conn: &Connection,
    parent_mission_id: &str,
) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT id FROM missions WHERE parent_mission_id = ?1 ORDER BY created_at ASC",
    )?;
    let rows = stmt
        .query_map([parent_mission_id], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
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

    // ---- FM-15 FR-05.x: planner fetch grants + counting ----

    #[test]
    fn ut15_fetch_grant_idempotent_and_scoped() {
        let conn = setup_db();
        create_planner_session(&conn, "ps1", None, "planner", "/tmp/r", "test").unwrap();
        create_planner_session(&conn, "ps2", None, "planner", "/tmp/r", "test").unwrap();

        record_planner_fetch_grant(&conn, "ps1", "example.com").unwrap();
        // 重复授权应当 idempotent
        record_planner_fetch_grant(&conn, "ps1", "example.com").unwrap();

        assert!(is_planner_fetch_granted(&conn, "ps1", "example.com").unwrap());
        // session 隔离
        assert!(!is_planner_fetch_granted(&conn, "ps2", "example.com").unwrap());
        // host 区分
        assert!(!is_planner_fetch_granted(&conn, "ps1", "evil.com").unwrap());
    }

    #[test]
    fn ut15_count_planner_fetch_calls_only_counts_fetch_url_calls() {
        let conn = setup_db();
        create_planner_session(&conn, "ps1", None, "planner", "/tmp/r", "test").unwrap();

        insert_planner_step(
            &conn, "s1", "ps1", 1, "tool_call", Some("read_file"), Some("{}"), None, None, 0,
        )
        .unwrap();
        insert_planner_step(
            &conn, "s2", "ps1", 2, "tool_call", Some("fetch_url"), Some("{}"), None, None, 0,
        )
        .unwrap();
        insert_planner_step(
            &conn, "s3", "ps1", 3, "tool_result", Some("fetch_url"), None, Some("{}"), None, 0,
        )
        .unwrap();
        insert_planner_step(
            &conn, "s4", "ps1", 4, "tool_call", Some("fetch_url"), Some("{}"), None, None, 0,
        )
        .unwrap();

        let n = count_planner_fetch_calls(&conn, "ps1").unwrap();
        assert_eq!(n, 2, "should count only tool_call rows for fetch_url");
    }

    // ---- FM-15 Phase 2 (FR-12 / FR-13) ----

    /// 主分支名读写应 idempotent；缺省值为 NULL。
    #[test]
    fn fm15_p2_main_branch_round_trip() {
        let conn = setup_db();
        create_mission(&conn, "m-mb");

        assert_eq!(get_mission_main_branch(&conn, "m-mb").unwrap(), None);

        set_mission_main_branch(&conn, "m-mb", "master").unwrap();
        assert_eq!(
            get_mission_main_branch(&conn, "m-mb").unwrap(),
            Some("master".to_string())
        );

        // 覆盖写入
        set_mission_main_branch(&conn, "m-mb", "main").unwrap();
        assert_eq!(
            get_mission_main_branch(&conn, "m-mb").unwrap(),
            Some("main".to_string())
        );
    }

    /// `count_running_agents_for_mission` 只统计本 mission 内的 running agent，
    /// 多 mission 共存时互不污染。
    #[test]
    fn fm15_p2_count_running_agents_isolated_per_mission() {
        let conn = setup_db();
        create_mission(&conn, "m-a");
        create_mission(&conn, "m-b");
        create_task(&conn, "t-a1", "m-a", "running");
        create_task(&conn, "t-a2", "m-a", "running");
        create_task(&conn, "t-b1", "m-b", "running");

        insert_agent_for_task(&conn, "ag-a1", "A1", "t-a1", "/tmp/wa1").unwrap();
        insert_agent_for_task(&conn, "ag-a2", "A2", "t-a2", "/tmp/wa2").unwrap();
        insert_agent_for_task(&conn, "ag-b1", "B1", "t-b1", "/tmp/wb1").unwrap();
        conn.execute(
            "UPDATE agents SET status = 'running' WHERE id IN ('ag-a1', 'ag-a2', 'ag-b1')",
            [],
        )
        .unwrap();

        assert_eq!(count_running_agents_for_mission(&conn, "m-a").unwrap(), 2);
        assert_eq!(count_running_agents_for_mission(&conn, "m-b").unwrap(), 1);
        // 全局口径仍然统计全部
        assert_eq!(count_running_agents(&conn).unwrap(), 3);
    }

    /// 非 running agent 不计入：idle / failed / cancelled 不应被算作占用槽位。
    #[test]
    fn fm15_p2_count_running_agents_ignores_non_running() {
        let conn = setup_db();
        create_mission(&conn, "m1");
        create_task(&conn, "t1", "m1", "running");
        create_task(&conn, "t2", "m1", "running");
        create_task(&conn, "t3", "m1", "running");

        insert_agent_for_task(&conn, "a1", "A1", "t1", "/tmp/w1").unwrap();
        insert_agent_for_task(&conn, "a2", "A2", "t2", "/tmp/w2").unwrap();
        insert_agent_for_task(&conn, "a3", "A3", "t3", "/tmp/w3").unwrap();

        conn.execute("UPDATE agents SET status = 'running' WHERE id = 'a1'", [])
            .unwrap();
        conn.execute("UPDATE agents SET status = 'failed' WHERE id = 'a2'", [])
            .unwrap();
        // a3 保持 'idle'

        assert_eq!(count_running_agents_for_mission(&conn, "m1").unwrap(), 1);
    }

    /// `use_incremental_worktree` 默认开启（schema DEFAULT 1），可被显式关闭。
    #[test]
    fn fm15_p2_use_incremental_worktree_defaults_on() {
        let conn = setup_db();
        create_mission(&conn, "m-on");
        assert!(get_mission_use_incremental_worktree(&conn, "m-on").unwrap());

        conn.execute(
            "UPDATE missions SET use_incremental_worktree = 0 WHERE id = 'm-on'",
            [],
        )
        .unwrap();
        assert!(!get_mission_use_incremental_worktree(&conn, "m-on").unwrap());
    }

    /// 辅助：把 task 标 completed + 关联 agent。
    fn complete_task(conn: &Connection, task_id: &str, agent_id: &str, ts: &str) {
        conn.execute(
            "INSERT INTO agents (id, name, task_id, status) VALUES (?1, ?1, ?2, 'completed')",
            rusqlite::params![agent_id, task_id],
        )
        .unwrap();
        conn.execute(
            "UPDATE tasks SET status = 'completed', assigned_agent_id = ?1, completed_at = ?2 WHERE id = ?3",
            rusqlite::params![agent_id, ts, task_id],
        )
        .unwrap();
    }

    /// FR-07: 拓扑后序父任务查询。菱形 A→{B,C}→D：D 的父应为 [B,C]，A 不算（不是直接父）。
    #[test]
    fn fm15_p2_completed_parents_excludes_indirect_ancestors() {
        let conn = setup_db();
        create_mission(&conn, "m1");
        create_task(&conn, "A", "m1", "pending");
        create_task(&conn, "B", "m1", "pending");
        create_task(&conn, "C", "m1", "pending");
        create_task(&conn, "D", "m1", "pending");
        add_dep(&conn, "B", "A");
        add_dep(&conn, "C", "A");
        add_dep(&conn, "D", "B");
        add_dep(&conn, "D", "C");

        complete_task(&conn, "A", "ag-A", "2026-04-01 00:00:00");
        complete_task(&conn, "B", "ag-B", "2026-04-01 00:00:01");
        complete_task(&conn, "C", "ag-C", "2026-04-01 00:00:02");

        let parents = get_completed_parent_tasks_for(&conn, "D").unwrap();
        let parent_ids: Vec<&str> = parents.iter().map(|(t, _)| t.as_str()).collect();
        assert!(parent_ids.contains(&"B"));
        assert!(parent_ids.contains(&"C"));
        assert!(!parent_ids.contains(&"A"), "A is indirect — must not appear");
        assert_eq!(parents.len(), 2);
    }

    /// FR-08.1: frontier merge —— 菱形 A→{B,C}→D 全部完成时，frontier 应只是 {D}。
    #[test]
    fn fm15_p2_frontier_picks_only_leaves() {
        let conn = setup_db();
        create_mission(&conn, "m1");
        for t in ["A", "B", "C", "D"] {
            create_task(&conn, t, "m1", "pending");
        }
        add_dep(&conn, "B", "A");
        add_dep(&conn, "C", "A");
        add_dep(&conn, "D", "B");
        add_dep(&conn, "D", "C");

        complete_task(&conn, "A", "ag-A", "2026-04-01 00:00:00");
        complete_task(&conn, "B", "ag-B", "2026-04-01 00:00:01");
        complete_task(&conn, "C", "ag-C", "2026-04-01 00:00:02");
        complete_task(&conn, "D", "ag-D", "2026-04-01 00:00:03");

        let frontier = get_frontier_completed_tasks(&conn, "m1").unwrap();
        let ids: Vec<&str> = frontier.iter().map(|(t, _, _)| t.as_str()).collect();
        assert_eq!(ids, vec!["D"], "only D has no completed successors");
    }

    /// FR-08.1: 多 frontier —— 线性 A→B + 独立 C 全完成时，frontier 应为 {B, C}。
    #[test]
    fn fm15_p2_frontier_handles_multiple_leaves() {
        let conn = setup_db();
        create_mission(&conn, "m1");
        for t in ["A", "B", "C"] {
            create_task(&conn, t, "m1", "pending");
        }
        add_dep(&conn, "B", "A");

        complete_task(&conn, "A", "ag-A", "2026-04-01 00:00:00");
        complete_task(&conn, "B", "ag-B", "2026-04-01 00:00:01");
        complete_task(&conn, "C", "ag-C", "2026-04-01 00:00:02");

        let frontier = get_frontier_completed_tasks(&conn, "m1").unwrap();
        let mut ids: Vec<&str> = frontier.iter().map(|(t, _, _)| t.as_str()).collect();
        ids.sort();
        assert_eq!(ids, vec!["B", "C"]);
    }

    /// FR-08.3: merge_records 写入与读取。
    #[test]
    fn fm15_p2_merge_records_round_trip() {
        let conn = setup_db();
        create_mission(&conn, "m1");

        record_merge_attempt(
            &conn,
            "rec-1",
            "m1",
            "agent/A",
            "main",
            "theirs",
            "auto",
            "[]",
            None,
            None,
            None,
        )
        .unwrap();
        record_merge_attempt(
            &conn,
            "rec-2",
            "m1",
            "agent/B",
            "main",
            "llm_resolve",
            "heuristic_theirs",
            "[\"shared.txt\"]",
            None,
            None,
            None,
        )
        .unwrap();

        let recs = get_merge_records_for_mission(&conn, "m1").unwrap();
        assert_eq!(recs.len(), 2);
        assert_eq!(recs[0].id, "rec-1");
        assert_eq!(recs[0].final_strategy, "auto");
        assert_eq!(recs[1].id, "rec-2");
        assert_eq!(recs[1].final_strategy, "heuristic_theirs");
        assert_eq!(recs[1].conflicted_files_json, "[\"shared.txt\"]");
    }

    /// FR-07.1: task_base_conflicts 写入与读取（INSERT OR REPLACE 幂等）。
    #[test]
    fn fm15_p2_task_base_conflicts_round_trip_and_idempotent() {
        let conn = setup_db();
        create_mission(&conn, "m1");
        create_task(&conn, "T-D", "m1", "pending");
        create_task(&conn, "T-B", "m1", "pending");
        create_task(&conn, "T-C", "m1", "pending");

        record_task_base_conflict(&conn, "T-D", "T-B", "shared.txt", "heuristic_theirs").unwrap();
        record_task_base_conflict(&conn, "T-D", "T-C", "shared.txt", "llm_failed_fallback")
            .unwrap();
        // 重复写入：覆盖上一条 resolution
        record_task_base_conflict(&conn, "T-D", "T-B", "shared.txt", "auto").unwrap();

        let conflicts = get_task_base_conflicts(&conn, "T-D").unwrap();
        assert_eq!(conflicts.len(), 2);

        // 排序：parent_task_id 升序
        assert_eq!(conflicts[0].0, "T-B");
        assert_eq!(conflicts[0].2, "auto", "later write should override");
        assert_eq!(conflicts[1].0, "T-C");
        assert_eq!(conflicts[1].2, "llm_failed_fallback");
    }
}
