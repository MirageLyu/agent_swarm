use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::{Emitter, Manager};
use uuid::Uuid;

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
    /// FM-15 v2.2 (S4): 暴露 Planner 写入的角色 / 富语义字段，便于前端
    /// RoleBadge / ArtifactBadge 展示。所有字段都尽量带默认值以兼容历史数据。
    #[serde(default = "default_task_role")]
    pub role: String,
    #[serde(default)]
    pub expected_output: Option<String>,
    /// 列表型字段：以 JSON 原始字符串透传，前端按需 parse；为 None 表示老 mission。
    #[serde(default)]
    pub additional_skills_json: Option<String>,
    #[serde(default)]
    pub produces_artifacts_json: Option<String>,
    #[serde(default)]
    pub consumes_artifacts_json: Option<String>,
    #[serde(default)]
    pub file_scope_hints_json: Option<String>,
}

fn default_task_role() -> String {
    "implementer".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DependencyInfo {
    pub task_id: String,
    pub depends_on: String,
    /// FM-15 v2.2 (S4): 该依赖边上承载的 artifact id 列表，JSON 字符串。
    /// None 表示尚未升级到富语义边的老依赖。
    #[serde(default)]
    pub artifact_refs_json: Option<String>,
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
    /// FM-15 v2.2 / FR-18: 必须显式声明 mission 的 repo origin。
    /// `None` 仅用于兼容尚未升级的内部调用（e.g. legacy import_mission_template）。
    /// 一旦 S3 全前端切换完毕，将变为 required。
    #[serde(default)]
    pub repo_origin: Option<String>,
    /// from_existing: 必填、必须是已存在目录；
    /// from_scratch:   可选、留空时由后端按 mission slug 生成 ~/miragenty-workspaces/...
    /// 缺省 repo_origin 时此字段被忽略。
    #[serde(default)]
    pub repo_path: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CreateMissionResponse {
    #[serde(flatten)]
    pub mission: MissionInfo,
    /// 实际落地的 repo_path（from_scratch 时是后端生成的）。
    pub repo_path: Option<String>,
    pub repo_origin: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct PlanMissionRequest {
    /// FM-15 v2.2 (S2): mission-first 模式。mission 必须已通过 `create_mission`
    /// 创建并带有有效 `repo_path`。
    pub mission_id: String,
}

#[derive(Debug, Serialize)]
pub struct PlanMissionResponse {
    pub mission_id: String,
    pub tasks: Vec<TaskInfo>,
    /// FM-15 v2.2: PlannerEngine 路径下产生的 session id；旧路径为 None
    #[serde(skip_serializing_if = "Option::is_none")]
    pub planner_session_id: Option<String>,
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
) -> Result<CreateMissionResponse, String> {
    let db = app.state::<crate::db::Database>();
    let id = Uuid::new_v4().to_string();

    // FM-15 v2.2 / FR-18: 解析 repo_origin 与 repo_path。
    // repo_origin = None 时走 legacy 路径（不写 repo_origin/repo_path），
    // 仅保留以兼容 import_mission_template 等内部调用；前端 S3 起会全量传值。
    let (repo_origin, repo_path) = match request.repo_origin.as_deref() {
        None => (None::<String>, None::<String>),
        Some("from_scratch") => {
            // path 由后端按 slug 自动派生，覆盖前端传值（避免歧义）
            let slug = crate::commands::agent::slugify(&request.title);
            let short_id = &id[..8.min(id.len())];
            let home = dirs::home_dir().ok_or("Cannot determine home directory")?;
            let path = home
                .join("miragenty-workspaces")
                .join(format!("{slug}-{short_id}"));
            crate::git::ensure_git_repo(&path).map_err(|e| {
                format!("Failed to init repo at '{}': {e}", path.display())
            })?;
            (
                Some("from_scratch".to_string()),
                Some(path.to_string_lossy().into_owned()),
            )
        }
        Some("from_existing") => {
            let raw = request
                .repo_path
                .as_deref()
                .ok_or("repo_path is required when repo_origin = from_existing")?
                .trim()
                .to_string();
            if raw.is_empty() {
                return Err("repo_path must not be empty for from_existing".into());
            }
            let path = std::path::PathBuf::from(&raw);
            if !path.exists() {
                return Err(format!("repo_path does not exist: {raw}"));
            }
            if !path.is_dir() {
                return Err(format!("repo_path is not a directory: {raw}"));
            }
            // 即便已是 git 仓库，ensure_git_repo 也是幂等的；非 git 目录会被自动 init。
            crate::git::ensure_git_repo(&path)
                .map_err(|e| format!("Failed to ensure git repo at '{raw}': {e}"))?;
            (
                Some("from_existing".to_string()),
                Some(path.to_string_lossy().into_owned()),
            )
        }
        Some(other) => {
            return Err(format!(
                "Invalid repo_origin '{other}', must be 'from_scratch' or 'from_existing'"
            ));
        }
    };

    let mission = db
        .with_conn(|conn| {
            conn.execute(
                "INSERT INTO missions (id, title, description, repo_origin, repo_path)
                 VALUES (?, ?, ?, ?, ?)",
                rusqlite::params![
                    id,
                    request.title,
                    request.description,
                    repo_origin,
                    repo_path
                ],
            )?;
            query_mission_info(conn, &id)
        })
        .map_err(|e| e.to_string())?;

    Ok(CreateMissionResponse {
        mission,
        repo_path,
        repo_origin,
    })
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
    let db = app.state::<crate::db::Database>();
    let mission_id = request.mission_id.clone();

    // FM-15 v2.2 (S2 / FR-05.1): mission-first。从 DB 读 description + repo_path。
    // 旧的「自带 description 自己 INSERT mission」路径已废弃。
    let (description, repo_path_str, status): (String, Option<String>, String) = db
        .with_conn(|conn| {
            conn.query_row(
                "SELECT description, repo_path, status FROM missions WHERE id = ?",
                [&mission_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .map_err(|e| anyhow::anyhow!("Mission {mission_id} not found: {e}"))
        })
        .map_err(|e| e.to_string())?;

    // 只允许 draft / preflight 进入 plan：planned/running/completed/failed 都禁止重 plan。
    if !matches!(status.as_str(), "draft" | "preflight") {
        return Err(format!(
            "Cannot plan mission in status '{status}'; only 'draft' / 'preflight' allowed"
        ));
    }
    let repo_path = repo_path_str.ok_or_else(|| {
        format!(
            "Mission {mission_id} has no repo_path. Re-create with FR-18 create_mission \
             (repo_origin + repo_path)."
        )
    })?;
    let repo_path = std::path::PathBuf::from(&repo_path);
    if !repo_path.is_dir() {
        return Err(format!(
            "Mission repo_path '{}' is not a directory",
            repo_path.display()
        ));
    }

    use crate::agent::planner_engine::PlannerEngine;
    let engine = PlannerEngine::new(provider, model.clone(), repo_path, app.clone());
    let outcome = engine
        .run(&description, Some(&mission_id))
        .await
        .map_err(|e| e.to_string())?;
    let planner_output = outcome.output;
    let planner_session_id = Some(outcome.session_id);

    let tasks = db
        .with_conn(|conn| {
            // 更新 mission 标题（plan 出的 title 通常比用户原始描述更准确）+ 状态
            conn.execute(
                "UPDATE missions SET title = ?, status = 'draft', updated_at = datetime('now')
                 WHERE id = ?",
                rusqlite::params![planner_output.mission_title, mission_id],
            )?;

            // 重 plan 时清掉旧 tasks（plan 阶段保证只有 draft/preflight 状态，无 running agent）
            conn.execute(
                "DELETE FROM task_dependencies WHERE task_id IN
                    (SELECT id FROM tasks WHERE mission_id = ?)",
                [&mission_id],
            )?;
            conn.execute("DELETE FROM tasks WHERE mission_id = ?", [&mission_id])?;

            let mut task_infos = Vec::new();
            let mut planner_id_to_db_id: std::collections::HashMap<String, String> =
                std::collections::HashMap::new();

            for pt in &planner_output.tasks {
                let task_id = Uuid::new_v4().to_string();
                planner_id_to_db_id.insert(pt.id.clone(), task_id.clone());

                // FM-15 v2.2 (S3-5): 富语义字段一并落库（JSON 字符串）。
                // consumes_artifacts 在 planner 层用 plan-id（T1.local_name），
                // 落库前翻译成 DB-uuid，让下游运行时/前端拿到稳定 id。
                // 因 planner_state 强制依赖必须先存在，producer 一定已经在 map 里。
                let consumes_translated: Vec<String> = pt
                    .consumes_artifacts
                    .iter()
                    .map(|c| {
                        if let Some((producer, local)) = c.split_once('.') {
                            if let Some(db) = planner_id_to_db_id.get(producer) {
                                return format!("{db}.{local}");
                            }
                        }
                        c.clone()
                    })
                    .collect();

                let additional_skills_json =
                    serde_json::to_string(&pt.additional_skills).unwrap_or_else(|_| "[]".into());
                let consumes_json =
                    serde_json::to_string(&consumes_translated).unwrap_or_else(|_| "[]".into());
                let produces_json =
                    serde_json::to_string(&pt.produces_artifacts).unwrap_or_else(|_| "[]".into());
                let file_scope_json =
                    serde_json::to_string(&pt.file_scope_hints).unwrap_or_else(|_| {
                        "{\"definite\":[],\"possible\":[]}".into()
                    });

                conn.execute(
                    "INSERT INTO tasks (id, mission_id, title, description, complexity, status, role, expected_output,
                        additional_skills, consumes_artifacts, produces_artifacts, file_scope_hints)
                     VALUES (?, ?, ?, ?, ?, 'pending', ?, ?, ?, ?, ?, ?)",
                    rusqlite::params![
                        task_id,
                        mission_id,
                        pt.title,
                        pt.description,
                        pt.complexity,
                        pt.effective_role(),
                        pt.effective_expected_output(),
                        additional_skills_json,
                        consumes_json,
                        produces_json,
                        file_scope_json,
                    ],
                )?;

                // 把每个声明 artifact 落入 artifacts 表（published=0）。
                for decl in &pt.produces_artifacts {
                    crate::agent::artifacts::record_declaration(
                        conn,
                        &mission_id,
                        &task_id,
                        decl,
                    )
                    .map_err(|e| {
                        rusqlite::Error::ToSqlConversionFailure(Box::new(
                            std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()),
                        ))
                    })?;
                }

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
                    role: pt.effective_role().to_string(),
                    expected_output: {
                        let eo = pt.effective_expected_output().to_string();
                        if eo.is_empty() { None } else { Some(eo) }
                    },
                    additional_skills_json: Some(additional_skills_json.clone()),
                    produces_artifacts_json: Some(produces_json.clone()),
                    consumes_artifacts_json: Some(consumes_json.clone()),
                    file_scope_hints_json: Some(file_scope_json.clone()),
                });
            }

            for pt in &planner_output.tasks {
                let task_db_id = &planner_id_to_db_id[&pt.id];
                for dep_planner_id in &pt.depends_on {
                    if let Some(dep_db_id) = planner_id_to_db_id.get(dep_planner_id) {
                        // FM-15 v2.2 (S3-5): 推导该边上的 artifact_refs：
                        // pt.consumes_artifacts 中所有 producer == dep_planner_id 的项，
                        // 同样翻译成 DB-uuid 形式（与 tasks.consumes_artifacts 一致）。
                        let plan_prefix = format!("{}.", dep_planner_id);
                        let refs: Vec<String> = pt
                            .consumes_artifacts
                            .iter()
                            .filter(|c| c.starts_with(&plan_prefix))
                            .map(|c| {
                                let local = c.trim_start_matches(&plan_prefix);
                                format!("{dep_db_id}.{local}")
                            })
                            .collect();
                        let refs_json =
                            serde_json::to_string(&refs).unwrap_or_else(|_| "[]".into());
                        conn.execute(
                            "INSERT INTO task_dependencies (task_id, depends_on, artifact_refs)
                             VALUES (?, ?, ?)",
                            rusqlite::params![task_db_id, dep_db_id, refs_json],
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

    if let Some(session_id) = planner_session_id.as_deref() {
        let _ = db.with_conn(|conn| {
            queries::link_planner_session_to_mission(conn, session_id, &mission_id)
        });
    }

    Ok(PlanMissionResponse {
        mission_id,
        tasks,
        planner_session_id,
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
                    assigned_agent_id, created_at, completed_at,
                    role, expected_output, additional_skills, produces_artifacts,
                    consumes_artifacts, file_scope_hints
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
                    role: row.get(9)?,
                    expected_output: row.get::<_, String>(10).ok().filter(|s| !s.is_empty()),
                    additional_skills_json: row.get(11).ok(),
                    produces_artifacts_json: row.get(12).ok(),
                    consumes_artifacts_json: row.get(13).ok(),
                    file_scope_hints_json: row.get(14).ok(),
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
                "SELECT task_id, depends_on, artifact_refs FROM task_dependencies WHERE task_id IN ({placeholders})"
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
                        artifact_refs_json: row.get(2).ok(),
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

#[derive(Debug, Deserialize)]
pub struct SetTaskDependenciesRequest {
    pub task_id: String,
    pub depends_on: Vec<String>,
}

#[tauri::command]
pub fn set_task_dependencies(
    app: tauri::AppHandle,
    request: SetTaskDependenciesRequest,
) -> Result<(), String> {
    let db = app.state::<crate::db::Database>();

    db.with_conn(|conn| {
        conn.execute(
            "DELETE FROM task_dependencies WHERE task_id = ?",
            [&request.task_id],
        )?;
        for dep_id in &request.depends_on {
            conn.execute(
                "INSERT INTO task_dependencies (task_id, depends_on) VALUES (?, ?)",
                rusqlite::params![&request.task_id, dep_id],
            )?;
        }
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
            role: default_task_role(),
            expected_output: None,
            additional_skills_json: None,
            produces_artifacts_json: None,
            consumes_artifacts_json: None,
            file_scope_hints_json: None,
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

// ---------- template export / import ----------

#[derive(Debug, Deserialize)]
pub struct ExportMissionTemplateRequest {
    pub mission_id: String,
    pub file_path: String,
}

#[tauri::command]
pub fn export_mission_template(
    app: tauri::AppHandle,
    request: ExportMissionTemplateRequest,
) -> Result<(), String> {
    let db = app.state::<crate::db::Database>();

    let detail = db
        .with_conn(|conn| {
            let mission = query_mission_info(conn, &request.mission_id)?;

            let mut task_stmt = conn.prepare(
                "SELECT id, mission_id, title, description, status, complexity,
                        assigned_agent_id, created_at, completed_at,
                        role, expected_output, additional_skills, produces_artifacts,
                        consumes_artifacts, file_scope_hints
                 FROM tasks WHERE mission_id = ? ORDER BY created_at ASC",
            )?;
            let tasks: Vec<TaskInfo> = task_stmt
                .query_map([&request.mission_id], |row| {
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
                        role: row.get(9)?,
                        expected_output: row.get::<_, String>(10).ok().filter(|s| !s.is_empty()),
                        additional_skills_json: row.get(11).ok(),
                        produces_artifacts_json: row.get(12).ok(),
                        consumes_artifacts_json: row.get(13).ok(),
                        file_scope_hints_json: row.get(14).ok(),
                    })
                })?
                .filter_map(|r| r.ok())
                .collect();

            let task_ids: Vec<String> = tasks.iter().map(|t| t.id.clone()).collect();
            let placeholders = task_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");

            let dependencies: Vec<DependencyInfo> = if task_ids.is_empty() {
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
                            artifact_refs_json: None,
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
        .map_err(|e| e.to_string())?;

    let template = crate::mission_template::build_template(&detail);
    let yaml =
        crate::mission_template::serialize_yaml(&template).map_err(|e| e.to_string())?;

    std::fs::write(&request.file_path, &yaml).map_err(|e| {
        format!("Failed to write file '{}': {e}", request.file_path)
    })?;

    tracing::info!(
        "Exported mission {} to {}",
        request.mission_id,
        request.file_path
    );
    Ok(())
}

#[tauri::command]
pub fn import_mission_template(
    app: tauri::AppHandle,
    file_path: String,
) -> Result<MissionInfo, String> {
    let yaml = std::fs::read_to_string(&file_path)
        .map_err(|e| format!("Failed to read file '{file_path}': {e}"))?;

    let template =
        crate::mission_template::parse_and_validate_yaml(&yaml).map_err(|e| e.to_string())?;

    let (title, description, tasks_with_uuid, dependencies) =
        crate::mission_template::template_to_db_records(&template);

    let db = app.state::<crate::db::Database>();
    let mission_id = Uuid::new_v4().to_string();

    db.with_conn(|conn| {
        conn.execute(
            "INSERT INTO missions (id, title, description, status) VALUES (?, ?, ?, 'draft')",
            rusqlite::params![mission_id, title, description],
        )?;

        for (uuid, task) in &tasks_with_uuid {
            conn.execute(
                "INSERT INTO tasks (id, mission_id, title, description, complexity, status)
                 VALUES (?, ?, ?, ?, ?, 'pending')",
                rusqlite::params![uuid, mission_id, task.title, task.description, task.complexity],
            )?;
        }

        for (task_uuid, dep_uuid) in &dependencies {
            conn.execute(
                "INSERT INTO task_dependencies (task_id, depends_on) VALUES (?, ?)",
                rusqlite::params![task_uuid, dep_uuid],
            )?;
        }

        query_mission_info(conn, &mission_id)
    })
    .map_err(|e| e.to_string())
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

// ---------- FM-15 v2.2 Slice 1: Planner Loop session inspection ----------

#[tauri::command]
pub fn get_planner_session(
    app: tauri::AppHandle,
    session_id: String,
) -> Result<Option<queries::PlannerSessionRow>, String> {
    let db = app.state::<crate::db::Database>();
    db.with_conn(|conn| queries::get_planner_session(conn, &session_id))
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn list_planner_steps(
    app: tauri::AppHandle,
    session_id: String,
) -> Result<Vec<queries::PlannerStepRow>, String> {
    let db = app.state::<crate::db::Database>();
    db.with_conn(|conn| queries::list_planner_steps(conn, &session_id))
        .map_err(|e| e.to_string())
}

// ---- FM-15 Phase 2 (FR-08.3 / FR-07.1): merge records & task base conflicts ----

#[tauri::command]
pub fn list_merge_records(
    app: tauri::AppHandle,
    mission_id: String,
) -> Result<Vec<queries::MergeRecord>, String> {
    let db = app.state::<crate::db::Database>();
    db.with_conn(|conn| queries::get_merge_records_for_mission(conn, &mission_id))
        .map_err(|e| e.to_string())
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskBaseConflictRow {
    pub parent_task_id: String,
    pub file_path: String,
    pub resolution: String,
}

#[tauri::command]
pub fn list_task_base_conflicts(
    app: tauri::AppHandle,
    task_id: String,
) -> Result<Vec<TaskBaseConflictRow>, String> {
    let db = app.state::<crate::db::Database>();
    db.with_conn(|conn| queries::get_task_base_conflicts(conn, &task_id))
        .map(|rows| {
            rows.into_iter()
                .map(|(p, f, r)| TaskBaseConflictRow {
                    parent_task_id: p,
                    file_path: f,
                    resolution: r,
                })
                .collect()
        })
        .map_err(|e| e.to_string())
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
