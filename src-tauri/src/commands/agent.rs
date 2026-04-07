use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use tauri::{Emitter, Manager};
use uuid::Uuid;

use crate::agent::scheduler::{MissionStatusChangedPayload, Scheduler};
use crate::agent::{AgentEngine, AgentRegistry};
use crate::commands::ConfigManager;
use crate::db::{queries, Database};
use crate::llm::{AnthropicProvider, LlmProvider, OpenAICompatProvider};

// ---- Request / Response types ----

#[derive(Debug, Deserialize)]
pub struct RunAgentRequest {
    pub task_description: String,
    pub workspace_path: String,
}

#[derive(Debug, Deserialize)]
pub struct StartMissionRequest {
    pub mission_id: String,
    pub repo_path: String,
}

#[derive(Debug, Serialize, Clone)]
pub struct SchedulerStatus {
    pub active_agents: i64,
    pub ready_tasks: i64,
    pub blocked_tasks: i64,
}

#[derive(Debug, Serialize, Clone)]
pub struct MissionAgentInfo {
    pub id: String,
    pub name: String,
    pub task_id: Option<String>,
    pub status: String,
    pub worktree_path: Option<String>,
    pub current_step: u32,
    pub tokens_used: u64,
    pub cost_usd: f64,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Serialize, Clone)]
pub struct RunAgentResponse {
    pub agent_id: String,
    pub status: String,
}

#[derive(Debug, Serialize, Clone)]
pub struct AgentEventRecord {
    pub id: String,
    pub agent_id: String,
    pub step: u32,
    pub kind: String,
    pub content: String,
    pub created_at: String,
}

#[derive(Debug, Serialize, Clone)]
pub struct AgentDetail {
    pub id: String,
    pub name: String,
    pub status: String,
    pub current_step: u32,
    pub tokens_used: u64,
    pub cost_usd: f64,
    pub created_at: String,
    pub updated_at: String,
}

// ---- Commands ----

#[tauri::command]
pub async fn run_agent(
    app: tauri::AppHandle,
    request: RunAgentRequest,
) -> Result<RunAgentResponse, String> {
    let config_mgr = app.state::<ConfigManager>();
    let config = config_mgr.get_config_snapshot();

    let provider_key = if config.api_keys.contains_key(&config.provider) {
        &config.provider
    } else {
        "default"
    };

    let api_key = config_mgr.get_api_key(provider_key).ok_or_else(|| {
        format!(
            "API key not configured for provider '{}'. Go to Settings to add it.",
            config.provider
        )
    })?;

    let provider: Arc<dyn LlmProvider> = match config.provider.as_str() {
        "anthropic" => Arc::new(AnthropicProvider::new(api_key)),
        _ => Arc::new(OpenAICompatProvider::new(api_key, config.base_url.clone())),
    };

    let agent_id = Uuid::new_v4().to_string();
    let workspace = std::path::PathBuf::from(&request.workspace_path);
    let model = config.default_model.clone();

    let db = app.state::<Database>();
    db.with_conn(|conn| {
        conn.execute(
            "INSERT INTO agents (id, name, status) VALUES (?1, ?2, 'idle')",
            rusqlite::params![agent_id, format!("Agent {}", &agent_id[..8])],
        )?;
        Ok(())
    })
    .map_err(|e| format!("Failed to create agent record: {e}"))?;

    let registry = app.state::<AgentRegistry>();
    let cancel_token = registry.register(&agent_id);

    let engine = AgentEngine::new(provider, workspace, app.app_handle().clone(), cancel_token);

    let id = agent_id.clone();
    let desc = request.task_description.clone();
    let app_clone = app.app_handle().clone();

    tokio::spawn(async move {
        let result = engine.run(&id, &desc, &model, 20).await;
        match &result {
            Ok(status) => {
                tracing::info!("Agent {id} finished with status: {status:?}");
            }
            Err(e) => {
                tracing::error!("Agent {id} error: {e}");
                let db = app_clone.state::<Database>();
                let _ = db.with_conn(|conn| {
                    conn.execute(
                        "UPDATE agents SET status = 'failed', updated_at = datetime('now') WHERE id = ?1",
                        rusqlite::params![id],
                    )?;
                    Ok(())
                });
            }
        }
        let registry = app_clone.state::<AgentRegistry>();
        registry.remove(&id);
    });

    Ok(RunAgentResponse {
        agent_id,
        status: "started".to_string(),
    })
}

#[tauri::command]
pub fn stop_agent(app: tauri::AppHandle, agent_id: String) -> Result<(), String> {
    let registry = app.state::<AgentRegistry>();
    registry.cancel(&agent_id);
    Ok(())
}

#[tauri::command]
pub fn get_agent_events(
    app: tauri::AppHandle,
    agent_id: String,
) -> Result<Vec<AgentEventRecord>, String> {
    let db = app.state::<Database>();
    db.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, agent_id, step, kind, content, created_at
             FROM agent_events WHERE agent_id = ?1 ORDER BY created_at ASC",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![agent_id], |row| {
                Ok(AgentEventRecord {
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
    })
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_agent_detail(
    app: tauri::AppHandle,
    agent_id: String,
) -> Result<AgentDetail, String> {
    let db = app.state::<Database>();
    db.with_conn(|conn| {
        conn.query_row(
            "SELECT id, name, status, current_step, tokens_used, cost_usd, created_at, updated_at
             FROM agents WHERE id = ?1",
            rusqlite::params![agent_id],
            |row| {
                Ok(AgentDetail {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    status: row.get(2)?,
                    current_step: row.get(3)?,
                    tokens_used: row.get(4)?,
                    cost_usd: row.get(5)?,
                    created_at: row.get(6)?,
                    updated_at: row.get(7)?,
                })
            },
        )
        .map_err(|e| anyhow::anyhow!("Agent not found: {e}"))
    })
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn list_agents(app: tauri::AppHandle) -> Result<Vec<AgentDetail>, String> {
    let db = app.state::<Database>();
    db.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, name, status, current_step, tokens_used, cost_usd, created_at, updated_at
             FROM agents ORDER BY created_at DESC LIMIT 50",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok(AgentDetail {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    status: row.get(2)?,
                    current_step: row.get(3)?,
                    tokens_used: row.get(4)?,
                    cost_usd: row.get(5)?,
                    created_at: row.get(6)?,
                    updated_at: row.get(7)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    })
    .map_err(|e| e.to_string())
}

// ---- FM-04: Activity stream & cost tracking commands ----

#[derive(Debug, Deserialize)]
pub struct ListAgentEventsRequest {
    pub mission_id: Option<String>,
    pub agent_id: Option<String>,
}

#[derive(Debug, Serialize, Clone)]
pub struct MissionCostSummary {
    pub total_cost: f64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
}

#[tauri::command]
pub fn list_agent_events(
    app: tauri::AppHandle,
    request: ListAgentEventsRequest,
) -> Result<Vec<AgentEventRecord>, String> {
    let db = app.state::<Database>();
    db.with_conn(|conn| {
        let rows = queries::list_agent_events(
            conn,
            request.mission_id.as_deref(),
            request.agent_id.as_deref(),
        )?;
        Ok(rows
            .into_iter()
            .map(|r| AgentEventRecord {
                id: r.id,
                agent_id: r.agent_id,
                step: r.step as u32,
                kind: r.kind,
                content: r.content,
                created_at: r.created_at,
            })
            .collect())
    })
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_mission_cost_summary(
    app: tauri::AppHandle,
    mission_id: String,
) -> Result<MissionCostSummary, String> {
    let db = app.state::<Database>();
    db.with_conn(|conn| {
        let summary = queries::get_mission_cost_summary(conn, &mission_id)?;
        Ok(MissionCostSummary {
            total_cost: summary.total_cost,
            total_input_tokens: summary.total_input_tokens,
            total_output_tokens: summary.total_output_tokens,
        })
    })
    .map_err(|e| e.to_string())
}

// ---- FM-02: Mission execution commands ----

#[tauri::command]
pub async fn start_mission_execution(
    app: tauri::AppHandle,
    request: StartMissionRequest,
) -> Result<(), String> {
    let repo_path = PathBuf::from(&request.repo_path);

    // Auto-create directory if it doesn't exist
    std::fs::create_dir_all(&repo_path)
        .map_err(|e| format!("Failed to create directory '{}': {e}", request.repo_path))?;

    // Auto git-init + initial commit if the directory isn't a git repo.
    // Worktrees require at least one commit (HEAD must not be unborn).
    if git2::Repository::open(&repo_path).is_err() {
        let repo = git2::Repository::init(&repo_path)
            .map_err(|e| format!("Failed to initialize git repository: {e}"))?;
        let sig = repo
            .signature()
            .or_else(|_| git2::Signature::now("Miragenty", "miragenty@localhost"))
            .map_err(|e| format!("Failed to create git signature: {e}"))?;
        let tree_id = repo
            .index()
            .and_then(|mut idx| idx.write_tree())
            .map_err(|e| format!("Failed to write tree: {e}"))?;
        let tree = repo
            .find_tree(tree_id)
            .map_err(|e| format!("Failed to find tree: {e}"))?;
        repo.commit(Some("HEAD"), &sig, &sig, "Initial commit", &tree, &[])
            .map_err(|e| format!("Failed to create initial commit: {e}"))?;
        tracing::info!("Auto-initialized git repo at {}", repo_path.display());
    }

    let db = app.state::<Database>();

    let mission_status: String = db
        .with_conn(|conn| {
            conn.query_row(
                "SELECT status FROM missions WHERE id = ?1",
                [&request.mission_id],
                |row| row.get(0),
            )
            .map_err(|_| anyhow::anyhow!("Mission not found"))
        })
        .map_err(|e| e.to_string())?;

    if mission_status != "planned" && mission_status != "running" {
        return Err(format!(
            "Mission must be in 'planned' or 'running' state to start (current: {mission_status})"
        ));
    }

    let old_status = mission_status.clone();

    if mission_status == "planned" {
        db.with_conn(|conn| {
            conn.execute(
                "UPDATE missions SET status = 'running', repo_path = ?1, updated_at = datetime('now') WHERE id = ?2",
                rusqlite::params![request.repo_path, request.mission_id],
            )?;
            Ok(())
        })
        .map_err(|e| e.to_string())?;
    } else {
        // running state — still record repo_path if not set
        db.with_conn(|conn| {
            conn.execute(
                "UPDATE missions SET repo_path = COALESCE(repo_path, ?1), updated_at = datetime('now') WHERE id = ?2",
                rusqlite::params![request.repo_path, request.mission_id],
            )?;
            Ok(())
        })
        .map_err(|e| e.to_string())?;
    }

    if mission_status == "running" {
        let reset = db
            .with_conn(|conn| queries::reset_orphaned_running_tasks(conn, &request.mission_id))
            .map_err(|e| e.to_string())?;
        if reset > 0 {
            tracing::info!("Reset {reset} orphaned running tasks for mission {}", request.mission_id);
        }
    }

    let worktrees_dir = repo_path.join(".worktrees");
    std::fs::create_dir_all(&worktrees_dir)
        .map_err(|e| format!("Failed to create .worktrees directory: {e}"))?;

    let scheduler = app.state::<Scheduler>();
    scheduler
        .start_mission(&request.mission_id, repo_path, app.clone())
        .map_err(|e| e.to_string())?;

    if old_status == "planned" {
        let _ = app.emit(
            "mission-status-changed",
            MissionStatusChangedPayload {
                mission_id: request.mission_id,
                from: "planned".to_string(),
                to: "running".to_string(),
            },
        );
    }

    Ok(())
}

#[derive(Debug, Serialize, Clone)]
pub struct DefaultWorkspacePath {
    pub path: String,
}

#[tauri::command]
pub fn get_default_workspace_path(
    app: tauri::AppHandle,
    mission_id: String,
) -> Result<DefaultWorkspacePath, String> {
    let db = app.state::<Database>();

    let title: String = db
        .with_conn(|conn| {
            conn.query_row(
                "SELECT title FROM missions WHERE id = ?1",
                [&mission_id],
                |row| row.get(0),
            )
            .map_err(|_| anyhow::anyhow!("Mission not found"))
        })
        .map_err(|e| e.to_string())?;

    let slug = slugify(&title);
    let short_id = &mission_id[..8.min(mission_id.len())];

    let home = dirs::home_dir().ok_or("Cannot determine home directory")?;
    let workspace_path = home
        .join("miragenty-workspaces")
        .join(format!("{slug}-{short_id}"));

    Ok(DefaultWorkspacePath {
        path: workspace_path.to_string_lossy().to_string(),
    })
}

fn slugify(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|seg| !seg.is_empty())
        .collect::<Vec<_>>()
        .join("-")
        .chars()
        .take(40)
        .collect()
}

#[tauri::command]
pub fn get_scheduler_status(app: tauri::AppHandle) -> Result<SchedulerStatus, String> {
    let db = app.state::<Database>();
    db.with_conn(|conn| {
        let active_agents: i64 = conn.query_row(
            "SELECT COUNT(*) FROM agents WHERE status = 'running'",
            [],
            |row| row.get(0),
        )?;
        let ready_tasks: i64 = conn.query_row(
            "SELECT COUNT(*) FROM tasks WHERE status = 'ready'",
            [],
            |row| row.get(0),
        )?;
        let blocked_tasks: i64 = conn.query_row(
            "SELECT COUNT(DISTINCT t.id) FROM tasks t
             JOIN task_dependencies td ON td.task_id = t.id
             JOIN tasks dep ON dep.id = td.depends_on
             WHERE t.status = 'pending' AND dep.status IN ('failed', 'cancelled')",
            [],
            |row| row.get(0),
        )?;
        Ok(SchedulerStatus {
            active_agents,
            ready_tasks,
            blocked_tasks,
        })
    })
    .map_err(|e| e.to_string())
}

// ---- FM-06: Runtime Intervention commands ----

#[derive(Debug, Deserialize)]
pub struct InjectAgentNoteRequest {
    pub agent_id: String,
    pub note: String,
}

#[derive(Debug, Serialize, Clone)]
pub struct InjectAgentNoteResponse {
    pub note_id: String,
}

#[derive(Debug, Serialize, Clone)]
pub struct AgentNoteRecord {
    pub id: String,
    pub agent_id: String,
    pub content: String,
    pub status: String,
    pub created_at: String,
    pub applied_at: Option<String>,
    pub mission_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct InjectMissionNoteRequest {
    pub mission_id: String,
    pub note: String,
}

#[derive(Debug, Serialize, Clone)]
pub struct InjectMissionNoteResponse {
    pub note_ids: Vec<String>,
    pub agent_count: usize,
}

#[tauri::command]
pub fn inject_agent_note(
    app: tauri::AppHandle,
    request: InjectAgentNoteRequest,
) -> Result<InjectAgentNoteResponse, String> {
    let note = request.note.trim().to_string();
    if note.is_empty() {
        return Err("Note content cannot be empty".to_string());
    }
    if note.len() > 2000 {
        return Err("Note content too long (max 2000 characters)".to_string());
    }

    let db = app.state::<Database>();

    let agent_status: String = db
        .with_conn(|conn| {
            conn.query_row(
                "SELECT status FROM agents WHERE id = ?1",
                rusqlite::params![request.agent_id],
                |row| row.get(0),
            )
            .map_err(|_| anyhow::anyhow!("Agent not found"))
        })
        .map_err(|e| e.to_string())?;

    if agent_status != "running" {
        return Err(format!(
            "Agent is not running (status: {agent_status})"
        ));
    }

    let note_id = Uuid::new_v4().to_string();
    db.with_conn(|conn| queries::insert_note(conn, &note_id, &request.agent_id, &note))
        .map_err(|e| format!("Failed to inject note: {e}"))?;

    Ok(InjectAgentNoteResponse { note_id })
}

#[tauri::command]
pub fn list_agent_notes(
    app: tauri::AppHandle,
    agent_id: String,
) -> Result<Vec<AgentNoteRecord>, String> {
    let db = app.state::<Database>();
    db.with_conn(|conn| {
        let rows = queries::list_notes_for_agent(conn, &agent_id)?;
        Ok(rows.into_iter().map(note_row_to_record).collect())
    })
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn inject_mission_note(
    app: tauri::AppHandle,
    request: InjectMissionNoteRequest,
) -> Result<InjectMissionNoteResponse, String> {
    let note = request.note.trim().to_string();
    if note.is_empty() {
        return Err("Note content cannot be empty".to_string());
    }
    if note.len() > 2000 {
        return Err("Note content too long (max 2000 characters)".to_string());
    }

    let db = app.state::<Database>();

    let running_agents = db
        .with_conn(|conn| queries::get_running_agent_ids_for_mission(conn, &request.mission_id))
        .map_err(|e| e.to_string())?;

    if running_agents.is_empty() {
        return Err("No running agents in this mission".to_string());
    }

    let mut note_ids = Vec::with_capacity(running_agents.len());
    let agent_count = running_agents.len();

    db.with_conn(|conn| {
        queries::append_mission_directive(conn, &request.mission_id, &note)?;

        for agent_id in &running_agents {
            let note_id = Uuid::new_v4().to_string();
            queries::insert_note_for_mission(
                conn,
                &note_id,
                agent_id,
                &request.mission_id,
                &note,
            )?;
            note_ids.push(note_id);
        }
        Ok(())
    })
    .map_err(|e| format!("Failed to inject mission note: {e}"))?;

    Ok(InjectMissionNoteResponse {
        note_ids,
        agent_count,
    })
}

#[tauri::command]
pub fn list_mission_notes(
    app: tauri::AppHandle,
    mission_id: String,
) -> Result<Vec<AgentNoteRecord>, String> {
    let db = app.state::<Database>();
    db.with_conn(|conn| {
        let rows = queries::list_notes_for_mission(conn, &mission_id)?;
        Ok(rows.into_iter().map(note_row_to_record).collect())
    })
    .map_err(|e| e.to_string())
}

fn note_row_to_record(r: queries::NoteRow) -> AgentNoteRecord {
    AgentNoteRecord {
        id: r.id,
        agent_id: r.agent_id,
        content: r.content,
        status: r.status,
        created_at: r.created_at,
        applied_at: r.applied_at,
        mission_id: r.mission_id,
    }
}

#[tauri::command]
pub fn list_agents_by_mission(
    app: tauri::AppHandle,
    mission_id: String,
) -> Result<Vec<MissionAgentInfo>, String> {
    let db = app.state::<Database>();
    db.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT a.id, a.name, a.task_id, a.status, a.worktree_path,
                    a.current_step, a.tokens_used, a.cost_usd, a.created_at, a.updated_at
             FROM agents a
             JOIN tasks t ON a.task_id = t.id
             WHERE t.mission_id = ?1
             ORDER BY a.created_at DESC",
        )?;
        let rows = stmt
            .query_map([&mission_id], |row| {
                Ok(MissionAgentInfo {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    task_id: row.get(2)?,
                    status: row.get(3)?,
                    worktree_path: row.get(4)?,
                    current_step: row.get(5)?,
                    tokens_used: row.get(6)?,
                    cost_usd: row.get(7)?,
                    created_at: row.get(8)?,
                    updated_at: row.get(9)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    })
    .map_err(|e| e.to_string())
}
