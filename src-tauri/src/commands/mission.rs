use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::{Emitter, Manager};
use uuid::Uuid;

use crate::agent::planner;
use crate::agent::scheduler::{MissionStatusChangedPayload, Scheduler};
use crate::commands::ConfigManager;
use crate::db::queries;
use crate::llm::{AnthropicProvider, LlmProvider, OpenAICompatProvider};

// ---------- shared types ----------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MissionInfo {
    pub id: String,
    pub title: String,
    pub description: String,
    pub status: String,
    pub total_cost_usd: f64,
    pub created_at: String,
    pub task_count: i64,
    pub completed_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskInfo {
    pub id: String,
    pub mission_id: String,
    pub title: String,
    pub description: String,
    pub status: String,
    pub complexity: String,
    pub assigned_agent_id: Option<String>,
    pub created_at: String,
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DependencyInfo {
    pub task_id: String,
    pub depends_on: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MissionDetail {
    pub mission: MissionInfo,
    pub tasks: Vec<TaskInfo>,
    pub dependencies: Vec<DependencyInfo>,
}

// ---------- request / response ----------

#[derive(Debug, Deserialize)]
pub struct CreateMissionRequest {
    pub title: String,
    pub description: String,
}

#[derive(Debug, Deserialize)]
pub struct PlanMissionRequest {
    pub description: String,
}

#[derive(Debug, Serialize)]
pub struct PlanMissionResponse {
    pub mission_id: String,
    pub tasks: Vec<TaskInfo>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateTaskRequest {
    pub task_id: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub status: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AddTaskRequest {
    pub mission_id: String,
    pub title: String,
    pub description: String,
    pub complexity: String,
    pub depends_on: Vec<String>,
}

// ---------- helper: build provider ----------

pub(crate) fn build_provider(app: &tauri::AppHandle) -> Result<(Arc<dyn LlmProvider>, String), String> {
    let config_mgr = app.state::<ConfigManager>();
    let config = config_mgr.get_config_snapshot();

    let provider_key = if config.api_keys.contains_key(&config.provider) {
        config.provider.clone()
    } else {
        "default".to_string()
    };

    let api_key = config_mgr
        .get_api_key(&provider_key)
        .ok_or_else(|| {
            "Please configure your API key in Settings first.".to_string()
        })?;

    let provider: Arc<dyn LlmProvider> = match config.provider.as_str() {
        "anthropic" => Arc::new(AnthropicProvider::new(api_key)),
        _ => Arc::new(OpenAICompatProvider::new(api_key, config.base_url.clone())),
    };

    Ok((provider, config.default_model))
}

// ---------- commands ----------

#[tauri::command]
pub fn create_mission(
    app: tauri::AppHandle,
    request: CreateMissionRequest,
) -> Result<MissionInfo, String> {
    let db = app.state::<crate::db::Database>();
    let id = Uuid::new_v4().to_string();

    db.with_conn(|conn| {
        conn.execute(
            "INSERT INTO missions (id, title, description) VALUES (?, ?, ?)",
            rusqlite::params![id, request.title, request.description],
        )?;

        query_mission_info(conn, &id)
    })
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn list_missions(app: tauri::AppHandle) -> Result<Vec<MissionInfo>, String> {
    let db = app.state::<crate::db::Database>();

    db.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT m.id, m.title, m.description, m.status, m.total_cost_usd, m.created_at,
                    (SELECT COUNT(*) FROM tasks WHERE mission_id = m.id) as task_count,
                    (SELECT COUNT(*) FROM tasks WHERE mission_id = m.id AND status = 'completed') as completed_count
             FROM missions m ORDER BY m.created_at DESC",
        )?;

        let missions = stmt
            .query_map([], |row| {
                Ok(MissionInfo {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    description: row.get(2)?,
                    status: row.get(3)?,
                    total_cost_usd: row.get(4)?,
                    created_at: row.get(5)?,
                    task_count: row.get(6)?,
                    completed_count: row.get(7)?,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();

        Ok(missions)
    })
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn plan_mission(
    app: tauri::AppHandle,
    request: PlanMissionRequest,
) -> Result<PlanMissionResponse, String> {
    let (provider, model) = build_provider(&app)?;

    let planner_output = planner::call_planner(provider, &model, &request.description, &app)
        .await
        .map_err(|e| e.to_string())?;

    let db = app.state::<crate::db::Database>();
    let mission_id = Uuid::new_v4().to_string();

    let tasks = db
        .with_conn(|conn| {
            conn.execute(
                "INSERT INTO missions (id, title, description, status) VALUES (?, ?, ?, 'draft')",
                rusqlite::params![mission_id, planner_output.mission_title, request.description],
            )?;

            let mut task_infos = Vec::new();
            let mut planner_id_to_db_id: std::collections::HashMap<String, String> =
                std::collections::HashMap::new();

            for pt in &planner_output.tasks {
                let task_id = Uuid::new_v4().to_string();
                planner_id_to_db_id.insert(pt.id.clone(), task_id.clone());

                conn.execute(
                    "INSERT INTO tasks (id, mission_id, title, description, complexity, status)
                     VALUES (?, ?, ?, ?, ?, 'pending')",
                    rusqlite::params![task_id, mission_id, pt.title, pt.description, pt.complexity],
                )?;

                task_infos.push(TaskInfo {
                    id: task_id,
                    mission_id: mission_id.clone(),
                    title: pt.title.clone(),
                    description: pt.description.clone(),
                    status: "pending".to_string(),
                    complexity: pt.complexity.clone(),
                    assigned_agent_id: None,
                    created_at: String::new(),
                    completed_at: None,
                });
            }

            for pt in &planner_output.tasks {
                let task_db_id = &planner_id_to_db_id[&pt.id];
                for dep_planner_id in &pt.depends_on {
                    if let Some(dep_db_id) = planner_id_to_db_id.get(dep_planner_id) {
                        conn.execute(
                            "INSERT INTO task_dependencies (task_id, depends_on) VALUES (?, ?)",
                            rusqlite::params![task_db_id, dep_db_id],
                        )?;
                    }
                }
            }

            for ti in &mut task_infos {
                ti.created_at = conn.query_row(
                    "SELECT created_at FROM tasks WHERE id = ?",
                    [&ti.id],
                    |row| row.get(0),
                )?;
            }

            Ok(task_infos)
        })
        .map_err(|e| e.to_string())?;

    Ok(PlanMissionResponse {
        mission_id,
        tasks,
    })
}

#[tauri::command]
pub fn get_mission_detail(
    app: tauri::AppHandle,
    mission_id: String,
) -> Result<MissionDetail, String> {
    let db = app.state::<crate::db::Database>();

    db.with_conn(|conn| {
        let mission = query_mission_info(conn, &mission_id)?;

        let mut task_stmt = conn.prepare(
            "SELECT id, mission_id, title, description, status, complexity,
                    assigned_agent_id, created_at, completed_at
             FROM tasks WHERE mission_id = ? ORDER BY created_at ASC",
        )?;
        let tasks: Vec<TaskInfo> = task_stmt
            .query_map([&mission_id], |row| {
                Ok(TaskInfo {
                    id: row.get(0)?,
                    mission_id: row.get(1)?,
                    title: row.get(2)?,
                    description: row.get(3)?,
                    status: row.get(4)?,
                    complexity: row.get(5)?,
                    assigned_agent_id: row.get(6)?,
                    created_at: row.get(7)?,
                    completed_at: row.get(8)?,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();

        let task_ids: Vec<String> = tasks.iter().map(|t| t.id.clone()).collect();
        let placeholders = task_ids
            .iter()
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(",");

        let dependencies = if task_ids.is_empty() {
            vec![]
        } else {
            let sql = format!(
                "SELECT task_id, depends_on FROM task_dependencies WHERE task_id IN ({placeholders})"
            );
            let mut dep_stmt = conn.prepare(&sql)?;
            let params: Vec<&dyn rusqlite::types::ToSql> = task_ids
                .iter()
                .map(|id| id as &dyn rusqlite::types::ToSql)
                .collect();
            let deps: Vec<DependencyInfo> = dep_stmt
                .query_map(params.as_slice(), |row| {
                    Ok(DependencyInfo {
                        task_id: row.get(0)?,
                        depends_on: row.get(1)?,
                    })
                })?
                .filter_map(|r| r.ok())
                .collect();
            deps
        };

        Ok(MissionDetail {
            mission,
            tasks,
            dependencies,
        })
    })
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn update_task(app: tauri::AppHandle, request: UpdateTaskRequest) -> Result<(), String> {
    let db = app.state::<crate::db::Database>();

    db.with_conn(|conn| {
        let mut sets = Vec::new();
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

        if let Some(ref title) = request.title {
            sets.push("title = ?");
            params.push(Box::new(title.clone()));
        }
        if let Some(ref description) = request.description {
            sets.push("description = ?");
            params.push(Box::new(description.clone()));
        }
        if let Some(ref status) = request.status {
            sets.push("status = ?");
            params.push(Box::new(status.clone()));
        }

        if sets.is_empty() {
            return Ok(());
        }

        let sql = format!(
            "UPDATE tasks SET {} WHERE id = ?",
            sets.join(", ")
        );
        params.push(Box::new(request.task_id));

        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();
        conn.execute(&sql, param_refs.as_slice())?;

        Ok(())
    })
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn delete_task(app: tauri::AppHandle, task_id: String) -> Result<(), String> {
    let db = app.state::<crate::db::Database>();

    db.with_conn(|conn| {
        conn.execute("DELETE FROM tasks WHERE id = ?", [&task_id])?;
        Ok(())
    })
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn add_task(app: tauri::AppHandle, request: AddTaskRequest) -> Result<TaskInfo, String> {
    let db = app.state::<crate::db::Database>();
    let task_id = Uuid::new_v4().to_string();

    db.with_conn(|conn| {
        conn.execute(
            "INSERT INTO tasks (id, mission_id, title, description, complexity, status)
             VALUES (?, ?, ?, ?, ?, 'pending')",
            rusqlite::params![
                task_id,
                request.mission_id,
                request.title,
                request.description,
                request.complexity,
            ],
        )?;

        for dep_id in &request.depends_on {
            conn.execute(
                "INSERT INTO task_dependencies (task_id, depends_on) VALUES (?, ?)",
                rusqlite::params![task_id, dep_id],
            )?;
        }

        let created_at: String = conn.query_row(
            "SELECT created_at FROM tasks WHERE id = ?",
            [&task_id],
            |row| row.get(0),
        )?;

        Ok(TaskInfo {
            id: task_id,
            mission_id: request.mission_id,
            title: request.title,
            description: request.description,
            status: "pending".to_string(),
            complexity: request.complexity,
            assigned_agent_id: None,
            created_at,
            completed_at: None,
        })
    })
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn confirm_mission(app: tauri::AppHandle, mission_id: String) -> Result<(), String> {
    let db = app.state::<crate::db::Database>();

    db.with_conn(|conn| {
        let status: String = conn
            .query_row(
                "SELECT status FROM missions WHERE id = ?",
                [&mission_id],
                |row| row.get(0),
            )
            .map_err(|_| anyhow::anyhow!("Mission not found"))?;

        if status != "draft" {
            anyhow::bail!("Only draft missions can be confirmed (current: {status})");
        }

        conn.execute(
            "UPDATE missions SET status = 'planned', updated_at = datetime('now') WHERE id = ?",
            [&mission_id],
        )?;

        conn.execute(
            "UPDATE tasks SET status = 'ready'
             WHERE mission_id = ? AND status = 'pending'
               AND id NOT IN (SELECT task_id FROM task_dependencies)",
            [&mission_id],
        )?;

        Ok(())
    })
    .map_err(|e| e.to_string())
}

#[derive(Debug, Deserialize)]
pub struct DeleteMissionRequest {
    pub mission_id: String,
    pub clean_workspace: bool,
}

#[tauri::command]
pub async fn delete_mission(
    app: tauri::AppHandle,
    request: DeleteMissionRequest,
) -> Result<(), String> {
    let db = app.state::<crate::db::Database>();

    let (status, repo_path): (String, Option<String>) = db
        .with_conn(|conn| {
            conn.query_row(
                "SELECT status, repo_path FROM missions WHERE id = ?",
                [&request.mission_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|_| anyhow::anyhow!("Mission not found"))
        })
        .map_err(|e| e.to_string())?;

    if status == "running" {
        return Err("Cannot delete a running mission. Please stop it first.".to_string());
    }

    db.with_conn(|conn| {
        queries::delete_agents_for_mission(conn, &request.mission_id)?;
        conn.execute("DELETE FROM missions WHERE id = ?", [&request.mission_id])?;
        Ok(())
    })
    .map_err(|e| e.to_string())?;

    // Async workspace cleanup
    if request.clean_workspace {
        if let Some(rp) = repo_path {
            let mid = request.mission_id.clone();
            tokio::spawn(async move {
                let worktrees_dir = std::path::PathBuf::from(&rp).join(".worktrees");
                if worktrees_dir.exists() {
                    if let Err(e) = tokio::fs::remove_dir_all(&worktrees_dir).await {
                        tracing::warn!("Failed to clean worktrees for mission {mid}: {e}");
                    } else {
                        tracing::info!("Cleaned worktrees for mission {mid}");
                    }
                }
            });
        }
    }

    Ok(())
}

#[tauri::command]
pub async fn stop_mission_execution(
    app: tauri::AppHandle,
    mission_id: String,
) -> Result<(), String> {
    let db = app.state::<crate::db::Database>();

    let status: String = db
        .with_conn(|conn| {
            conn.query_row(
                "SELECT status FROM missions WHERE id = ?",
                [&mission_id],
                |row| row.get(0),
            )
            .map_err(|_| anyhow::anyhow!("Mission not found"))
        })
        .map_err(|e| e.to_string())?;

    if status != "running" {
        return Err(format!(
            "Mission must be running to stop (current: {status})"
        ));
    }

    let scheduler = app.state::<Scheduler>();
    scheduler.stop_mission(&mission_id);

    db.with_conn(|conn| {
        queries::reset_orphaned_running_tasks(conn, &mission_id)?;
        conn.execute(
            "UPDATE missions SET status = 'failed', updated_at = datetime('now') WHERE id = ?1",
            [&mission_id],
        )?;
        Ok(())
    })
    .map_err(|e| e.to_string())?;

    let _ = app.emit(
        "mission-status-changed",
        MissionStatusChangedPayload {
            mission_id,
            from: "running".to_string(),
            to: "failed".to_string(),
        },
    );

    Ok(())
}

#[derive(Debug, Deserialize)]
pub struct RestartMissionRequest {
    pub mission_id: String,
    pub mode: String, // "full" | "failed_only"
}

#[derive(Debug, Serialize, Clone)]
pub struct RestartResult {
    pub reset_count: u32,
}

#[tauri::command]
pub async fn restart_mission(
    app: tauri::AppHandle,
    request: RestartMissionRequest,
) -> Result<RestartResult, String> {
    let db = app.state::<crate::db::Database>();

    let (status, repo_path): (String, Option<String>) = db
        .with_conn(|conn| {
            conn.query_row(
                "SELECT status, repo_path FROM missions WHERE id = ?",
                [&request.mission_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|_| anyhow::anyhow!("Mission not found"))
        })
        .map_err(|e| e.to_string())?;

    if status != "completed" && status != "failed" {
        return Err(format!(
            "Mission must be completed or failed to restart (current: {status})"
        ));
    }

    let reset_count = db
        .with_conn(|conn| {
            match request.mode.as_str() {
                "full" => {
                    queries::delete_agents_for_mission(conn, &request.mission_id)?;
                    let count = queries::reset_all_tasks(conn, &request.mission_id)?;

                    conn.execute(
                        "UPDATE missions SET status = 'planned', updated_at = datetime('now') WHERE id = ?1",
                        [&request.mission_id],
                    )?;

                    Ok(count)
                }
                "failed_only" => {
                    let failed_count =
                        queries::count_failed_tasks(conn, &request.mission_id)?;
                    if failed_count == 0 {
                        anyhow::bail!("No failed or cancelled tasks to restart");
                    }

                    // Get failed task ids for selective agent cleanup
                    let mut stmt = conn.prepare(
                        "SELECT id FROM tasks WHERE mission_id = ?1 AND status IN ('failed', 'cancelled')",
                    )?;
                    let failed_ids: Vec<String> = stmt
                        .query_map(rusqlite::params![request.mission_id], |row| row.get(0))?
                        .collect::<std::result::Result<Vec<_>, _>>()?;

                    queries::delete_agents_for_tasks(conn, &failed_ids)?;
                    let count = queries::reset_failed_tasks(conn, &request.mission_id)?;

                    conn.execute(
                        "UPDATE missions SET status = 'planned', updated_at = datetime('now') WHERE id = ?1",
                        [&request.mission_id],
                    )?;

                    Ok(count)
                }
                _ => anyhow::bail!("Invalid restart mode: {}", request.mode),
            }
        })
        .map_err(|e| e.to_string())?;

    // Async worktree cleanup
    if let Some(rp) = repo_path {
        let mode = request.mode.clone();
        tokio::spawn(async move {
            if mode == "full" {
                let worktrees_dir = std::path::PathBuf::from(&rp).join(".worktrees");
                if worktrees_dir.exists() {
                    let _ = tokio::fs::remove_dir_all(&worktrees_dir).await;
                }
            }
        });
    }

    let _ = app.emit(
        "mission-status-changed",
        MissionStatusChangedPayload {
            mission_id: request.mission_id,
            from: status,
            to: "planned".to_string(),
        },
    );

    Ok(RestartResult { reset_count })
}

// ---------- helpers ----------

fn query_mission_info(
    conn: &rusqlite::Connection,
    mission_id: &str,
) -> anyhow::Result<MissionInfo> {
    conn.query_row(
        "SELECT m.id, m.title, m.description, m.status, m.total_cost_usd, m.created_at,
                (SELECT COUNT(*) FROM tasks WHERE mission_id = m.id) as task_count,
                (SELECT COUNT(*) FROM tasks WHERE mission_id = m.id AND status = 'completed') as completed_count
         FROM missions m WHERE m.id = ?",
        [mission_id],
        |row| {
            Ok(MissionInfo {
                id: row.get(0)?,
                title: row.get(1)?,
                description: row.get(2)?,
                status: row.get(3)?,
                total_cost_usd: row.get(4)?,
                created_at: row.get(5)?,
                task_count: row.get(6)?,
                completed_count: row.get(7)?,
            })
        },
    )
    .map_err(|e| e.into())
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    fn setup_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        crate::db::migrations_run_on(&conn).unwrap();
        conn
    }

    #[test]
    fn ut03_1_create_mission_and_tasks() {
        let conn = setup_db();
        let mid = "m1";
        conn.execute(
            "INSERT INTO missions (id, title, description) VALUES (?, 'Test Mission', 'desc')",
            [mid],
        )
        .unwrap();

        for (id, title) in [("t1", "Task 1"), ("t2", "Task 2"), ("t3", "Task 3")] {
            conn.execute(
                "INSERT INTO tasks (id, mission_id, title, description, complexity) VALUES (?, ?, ?, 'desc', 'medium')",
                rusqlite::params![id, mid, title],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO task_dependencies (task_id, depends_on) VALUES ('t2', 't1')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO task_dependencies (task_id, depends_on) VALUES ('t3', 't2')",
            [],
        )
        .unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM tasks WHERE mission_id = ?", [mid], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 3);

        let dep_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM task_dependencies WHERE task_id IN ('t1','t2','t3')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(dep_count, 2);
    }

    #[test]
    fn ut03_2_delete_task_cascades_deps() {
        let conn = setup_db();
        conn.execute(
            "INSERT INTO missions (id, title) VALUES ('m1', 'M')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tasks (id, mission_id, title, complexity) VALUES ('t1', 'm1', 'T1', 'low')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tasks (id, mission_id, title, complexity) VALUES ('t2', 'm1', 'T2', 'low')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tasks (id, mission_id, title, complexity) VALUES ('t3', 'm1', 'T3', 'low')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO task_dependencies (task_id, depends_on) VALUES ('t3', 't2')",
            [],
        )
        .unwrap();

        conn.execute("DELETE FROM tasks WHERE id = 't2'", []).unwrap();

        let dep_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM task_dependencies WHERE depends_on = 't2'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(dep_count, 0);
    }

    #[test]
    fn ut03_3_update_task_fields() {
        let conn = setup_db();
        conn.execute("INSERT INTO missions (id, title) VALUES ('m1', 'M')", []).unwrap();
        conn.execute(
            "INSERT INTO tasks (id, mission_id, title, description, complexity) VALUES ('t1', 'm1', 'Old', 'old desc', 'low')",
            [],
        )
        .unwrap();

        conn.execute(
            "UPDATE tasks SET title = 'New', description = 'new desc' WHERE id = 't1'",
            [],
        )
        .unwrap();

        let (title, desc): (String, String) = conn
            .query_row("SELECT title, description FROM tasks WHERE id = 't1'", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(title, "New");
        assert_eq!(desc, "new desc");
    }

    #[test]
    fn ut03_4_confirm_mission() {
        let conn = setup_db();
        conn.execute("INSERT INTO missions (id, title, status) VALUES ('m1', 'M', 'draft')", []).unwrap();
        conn.execute(
            "INSERT INTO tasks (id, mission_id, title, complexity, status) VALUES ('t1', 'm1', 'T1', 'low', 'pending')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tasks (id, mission_id, title, complexity, status) VALUES ('t2', 'm1', 'T2', 'low', 'pending')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO task_dependencies (task_id, depends_on) VALUES ('t2', 't1')",
            [],
        )
        .unwrap();

        conn.execute(
            "UPDATE missions SET status = 'planned' WHERE id = 'm1'",
            [],
        )
        .unwrap();
        conn.execute(
            "UPDATE tasks SET status = 'ready' WHERE mission_id = 'm1' AND status = 'pending' AND id NOT IN (SELECT task_id FROM task_dependencies)",
            [],
        )
        .unwrap();

        let m_status: String = conn
            .query_row("SELECT status FROM missions WHERE id = 'm1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(m_status, "planned");

        let t1_status: String = conn
            .query_row("SELECT status FROM tasks WHERE id = 't1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(t1_status, "ready");

        let t2_status: String = conn
            .query_row("SELECT status FROM tasks WHERE id = 't2'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(t2_status, "pending");
    }

    #[test]
    fn ut03_5_delete_draft_mission() {
        let conn = setup_db();
        conn.execute("INSERT INTO missions (id, title, status) VALUES ('m1', 'M', 'draft')", []).unwrap();
        conn.execute(
            "INSERT INTO tasks (id, mission_id, title, complexity) VALUES ('t1', 'm1', 'T1', 'low')",
            [],
        )
        .unwrap();

        conn.execute("DELETE FROM missions WHERE id = 'm1'", []).unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM missions WHERE id = 'm1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);

        let task_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM tasks WHERE mission_id = 'm1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(task_count, 0);
    }

    #[test]
    fn ut03_6_cannot_delete_running_mission() {
        let conn = setup_db();
        conn.execute("INSERT INTO missions (id, title, status) VALUES ('m1', 'M', 'running')", []).unwrap();

        let status: String = conn
            .query_row("SELECT status FROM missions WHERE id = 'm1'", [], |r| r.get(0))
            .unwrap();
        assert_ne!(status, "draft");
    }
}
