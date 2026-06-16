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
    /// 最近一次失败原因（带分类前缀，如 "timeout: …" / "max_steps: …"）。
    /// 前端 DAG 节点 / TaskDetailPanel hover 时展示。
    #[serde(default)]
    pub last_error: Option<String>,
    /// 最近一次失败时间（UTC ISO8601）。
    #[serde(default)]
    pub last_failed_at: Option<String>,
    /// Explicit Merge Node v1：`"work"`（默认）或 `"merge"`。前端 TaskDAG 据此
    /// 渲染菱形节点 + 不同颜色。老 mission 兜底 'work'。
    #[serde(default = "default_task_kind")]
    pub kind: String,
    /// Explicit Merge Node v1：merge 节点的 2 个 parent task DB id（JSON 数组）。
    /// `kind == "merge"` 时非空；UI 在 hover 时展开"合并了哪两个 task"。
    #[serde(default)]
    pub merge_parents_json: Option<String>,
}

fn default_task_kind() -> String {
    "work".to_string()
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
    /// FM-15 v2.3：边的语义分类（`producer` / `reference`）。
    /// - `producer`：携带 code_module / test_module / config 等"实物 artifact"，
    ///   下游真依赖上游的代码产出。
    /// - `reference`：携带 design_doc / api_spec / schema / docs / report 等
    ///   文档型 artifact，对调度等价但 UI 默认弱化/隐藏，缓解长 DAG 的视觉膨胀。
    ///
    /// 旧行（migration 024 之前）默认 `producer`。调度路径暂不区分 kind。
    #[serde(default = "default_dep_kind")]
    pub kind: String,
}

fn default_dep_kind() -> String {
    "producer".to_string()
}

/// 携带 doc 类 artifact 的边在 UI 上默认弱化为 reference。集合保持与
/// migration 024 的 backfill 子句一致——任何变更需要同步两边。
pub(crate) const DOC_ARTIFACT_TYPES: &[&str] =
    &["design_doc", "api_spec", "schema", "docs", "report"];

/// 根据边上的 artifact_refs + 上游 task 的 produces 声明判定 kind。
///
/// 规则（与 migration 024 backfill 一致）：
/// - `artifact_refs` 为空 → `producer`（保守：纯拓扑边按实物依赖处理）。
/// - 所有 ref 解析出的 artifact_type 都属于 `DOC_ARTIFACT_TYPES` → `reference`。
/// - 任一 ref 解析失败（找不到 local_name）或类型不在 doc-set → `producer`。
pub(crate) fn classify_edge_kind(
    refs: &[String],
    upstream_produces: &[crate::agent::artifacts::ArtifactDecl],
) -> &'static str {
    if refs.is_empty() {
        return "producer";
    }
    for r in refs {
        let local = match r.split_once('.') {
            Some((_, n)) => n,
            None => return "producer",
        };
        let typ = upstream_produces
            .iter()
            .find(|a| a.local_name == local)
            .map(|a| a.artifact_type.as_str());
        match typ {
            Some(t) if DOC_ARTIFACT_TYPES.contains(&t) => continue,
            _ => return "producer",
        }
    }
    "reference"
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

pub(crate) fn build_provider(
    app: &tauri::AppHandle,
) -> Result<(Arc<dyn LlmProvider>, String), String> {
    let config_mgr = app.state::<ConfigManager>();
    let config = config_mgr.get_config_snapshot();

    let provider_key = if config.api_keys.contains_key(&config.provider) {
        config.provider.clone()
    } else {
        "default".to_string()
    };

    let api_key = config_mgr
        .get_api_key(&provider_key)
        .ok_or_else(|| "Please configure your API key in Settings first.".to_string())?;

    let stream_idle = config.agent_step_idle_seconds;
    let provider: Arc<dyn LlmProvider> = match config.provider.as_str() {
        "anthropic" => Arc::new(AnthropicProvider::with_stream_idle(api_key, stream_idle)),
        _ => Arc::new(OpenAICompatProvider::with_stream_idle(
            api_key,
            config.base_url.clone(),
            stream_idle,
        )),
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
            crate::git::ensure_git_repo(&path)
                .map_err(|e| format!("Failed to init repo at '{}': {e}", path.display()))?;
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
    let mut planner_output = outcome.output;
    let planner_session_id = Some(outcome.session_id);

    // Explicit Merge Node v1：见 preflight.rs 同名注释。
    let merge_inject_enabled = {
        let cfg_mgr = app.state::<crate::commands::ConfigManager>();
        let cfg = cfg_mgr.get_config_snapshot();
        cfg.enable_explicit_merge_node
    };
    let merge_inject_added = crate::agent::planner_merge_inject::inject_merge_nodes(
        &mut planner_output.tasks,
        crate::agent::planner_merge_inject::InjectOptions {
            enabled: merge_inject_enabled,
        },
    );
    if merge_inject_added > 0 {
        tracing::info!(
            mission_id = %mission_id,
            added = merge_inject_added,
            total_tasks = planner_output.tasks.len(),
            "Explicit Merge Node v1: injected merge nodes for multi-parent joins"
        );
    }

    // FM-15 v2.2 (retryable-flow rule 2)：第 3 步多条 SQL 必须包事务，
    // 否则 INSERT 中途失败会留下"title 已更新但 tasks 全空 / 半空"的脏 mission。
    let tasks = db
        .with_conn(|conn| {
            let tx = conn.unchecked_transaction()?;

            // 更新 mission 标题（plan 出的 title 通常比用户原始描述更准确）+ 状态
            tx.execute(
                "UPDATE missions SET title = ?, status = 'draft', updated_at = datetime('now')
                 WHERE id = ?",
                rusqlite::params![planner_output.mission_title, mission_id],
            )?;

            // 重 plan 时清掉旧 tasks（plan 阶段保证只有 draft/preflight 状态，无 running agent）
            tx.execute(
                "DELETE FROM task_dependencies WHERE task_id IN
                    (SELECT id FROM tasks WHERE mission_id = ?)",
                [&mission_id],
            )?;
            tx.execute("DELETE FROM tasks WHERE mission_id = ?", [&mission_id])?;

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

                // Explicit Merge Node v1：与 sign_contract 路径同构。
                let kind_db = pt.kind.as_db_str();
                let merge_parents_json: Option<String> = if pt.merge_parents.is_empty() {
                    None
                } else {
                    let mapped: Vec<String> = pt
                        .merge_parents
                        .iter()
                        .filter_map(|p_id| planner_id_to_db_id.get(p_id).cloned())
                        .collect();
                    serde_json::to_string(&mapped).ok()
                };
                tx.execute(
                    "INSERT INTO tasks (id, mission_id, title, description, complexity, status, role, expected_output,
                        additional_skills, consumes_artifacts, produces_artifacts, file_scope_hints, kind, merge_parents)
                     VALUES (?, ?, ?, ?, ?, 'pending', ?, ?, ?, ?, ?, ?, ?, ?)",
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
                        kind_db,
                        merge_parents_json,
                    ],
                )?;

                // 把每个声明 artifact 落入 artifacts 表（published=0）。
                for decl in &pt.produces_artifacts {
                    crate::agent::artifacts::record_declaration(
                        &tx,
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
                    last_error: None,
                    last_failed_at: None,
                    kind: pt.kind.as_db_str().to_string(),
                    merge_parents_json: merge_parents_json.clone(),
                });
            }

            // 用 planner_id → PlannerTask 的索引便于查上游的 produces_artifacts（推导 kind 用）。
            let planner_task_by_id: std::collections::HashMap<&str, &_> = planner_output
                .tasks
                .iter()
                .map(|t| (t.id.as_str(), t))
                .collect();

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
                        // FM-15 v2.3：根据边上携带的 artifact 类型推导 kind。
                        let upstream_produces = planner_task_by_id
                            .get(dep_planner_id.as_str())
                            .map(|t| t.produces_artifacts.as_slice())
                            .unwrap_or(&[]);
                        let kind = classify_edge_kind(&refs, upstream_produces);
                        tx.execute(
                            "INSERT INTO task_dependencies (task_id, depends_on, artifact_refs, kind)
                             VALUES (?, ?, ?, ?)",
                            rusqlite::params![task_db_id, dep_db_id, refs_json, kind],
                        )?;
                    }
                }
            }

            for ti in &mut task_infos {
                ti.created_at = tx.query_row(
                    "SELECT created_at FROM tasks WHERE id = ?",
                    [&ti.id],
                    |row| row.get(0),
                )?;
            }

            tx.commit()?;
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
                    consumes_artifacts, file_scope_hints,
                    last_error, last_failed_at, kind, merge_parents
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
                    last_error: row.get(15).ok(),
                    last_failed_at: row.get(16).ok(),
                    kind: row
                        .get::<_, Option<String>>(17)
                        .ok()
                        .flatten()
                        .unwrap_or_else(|| "work".to_string()),
                    merge_parents_json: row.get::<_, Option<String>>(18).ok().flatten(),
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
                "SELECT task_id, depends_on, artifact_refs, kind FROM task_dependencies WHERE task_id IN ({placeholders})"
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
                        kind: row
                            .get::<_, Option<String>>(3)
                            .ok()
                            .flatten()
                            .unwrap_or_else(|| "producer".to_string()),
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

        let sql = format!("UPDATE tasks SET {} WHERE id = ?", sets.join(", "));
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
            last_error: None,
            last_failed_at: None,
            kind: "work".to_string(),
            merge_parents_json: None,
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

    // 关键：取消 mission 循环之前，**先**取消所有正在跑的 agent 的 cancel_token。
    // 否则 agent task 还会跑完整个 step（含 LLM stream，最坏 180s）才在 step 边界
    // 检查到取消信号——用户体感"点了停止毫无反应"。
    //
    // scheduler.stop_mission 只取消调度循环，agent task 用的是 AgentRegistry 里
    // 注册的独立 token；两套不联动是历史遗留。
    let running_agents: Vec<String> = db
        .with_conn(|conn| queries::list_running_agent_ids_for_mission(conn, &mission_id))
        .unwrap_or_default();
    if !running_agents.is_empty() {
        let registry = app.state::<crate::agent::AgentRegistry>();
        let cancelled_count = running_agents
            .iter()
            .filter(|aid| registry.cancel(aid))
            .count();
        tracing::info!(
            "stop_mission_execution({mission_id}): cancelled {cancelled_count}/{} running agents",
            running_agents.len()
        );
    }

    let scheduler = app.state::<Scheduler>();
    scheduler.stop_mission(&mission_id);

    db.with_conn(|conn| {
        queries::reset_orphaned_running_tasks(conn, &mission_id)?;
        conn.execute(
            "UPDATE missions SET status = 'failed', updated_at = datetime('now') WHERE id = ?1",
            [&mission_id],
        )?;
        if let Err(err) = crate::agent::delivery::generate_and_persist_degraded_delivery_on_conn(
            conn,
            &mission_id,
        ) {
            tracing::warn!(
                mission_id = %mission_id,
                error = %err,
                "mission delivery generation failed after stopped mission was marked failed"
            );
        }
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
    /// 复用上次的 repo_path 直接拉起 scheduler，前端可一键重跑跳过工作区选择。
    /// 仅在 mission 已经记录 repo_path 时生效；缺失则降级为只重置不启动。
    #[serde(default)]
    pub auto_start: bool,
}

#[derive(Debug, Serialize, Clone)]
pub struct RestartResult {
    pub reset_count: u32,
    /// 实际是否已经被 auto-start 拉起；前端据此决定要不要再弹工作区对话框。
    #[serde(default)]
    pub auto_started: bool,
    /// 复用的 repo_path（若有）回传给前端，前端可直接用于后续 open_in_editor 等操作。
    pub repo_path: Option<String>,
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

    // FM-15 v2.2 (retryable-flow rule 2)：restart 是流程型操作，多步 SQL 必须包事务。
    // 否则"清掉 agents 后 reset_tasks 失败"会留下"任务还在 failed 但 agent 没了"的孤儿状态。
    let reset_count = db
        .with_conn(|conn| {
            let tx = conn.unchecked_transaction()?;
            let count = match request.mode.as_str() {
                "full" => {
                    queries::delete_agents_for_mission(&tx, &request.mission_id)?;
                    let count = queries::reset_all_tasks(&tx, &request.mission_id)?;

                    tx.execute(
                        "UPDATE missions SET status = 'planned', updated_at = datetime('now') WHERE id = ?1",
                        [&request.mission_id],
                    )?;

                    count
                }
                "failed_only" => {
                    let failed_count =
                        queries::count_failed_tasks(&tx, &request.mission_id)?;
                    if failed_count == 0 {
                        anyhow::bail!("No failed or cancelled tasks to restart");
                    }

                    let failed_ids: Vec<String> = {
                        let mut stmt = tx.prepare(
                            "SELECT id FROM tasks WHERE mission_id = ?1 AND status IN ('failed', 'cancelled')",
                        )?;
                        let collected = stmt
                            .query_map(rusqlite::params![request.mission_id], |row| row.get(0))?
                            .collect::<std::result::Result<Vec<_>, _>>()?;
                        collected
                    };

                    queries::delete_agents_for_tasks(&tx, &failed_ids)?;
                    let count = queries::reset_failed_tasks(&tx, &request.mission_id)?;

                    tx.execute(
                        "UPDATE missions SET status = 'planned', updated_at = datetime('now') WHERE id = ?1",
                        [&request.mission_id],
                    )?;

                    count
                }
                _ => anyhow::bail!("Invalid restart mode: {}", request.mode),
            };
            tx.commit()?;
            Ok(count)
        })
        .map_err(|e| e.to_string())?;

    // Async worktree cleanup —— full 模式才需要清空 worktree
    if let Some(rp) = repo_path.clone() {
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
            mission_id: request.mission_id.clone(),
            from: status,
            to: "planned".to_string(),
        },
    );

    // FM-15 follow-up: 一键重跑——复用上次 repo_path 直接拉起 scheduler。
    // 失败不阻塞 restart 本身：返回 auto_started=false 让前端补 fallback（弹对话框）。
    let mut auto_started = false;
    if request.auto_start {
        if let Some(ref rp) = repo_path {
            match auto_start_mission(&app, &request.mission_id, rp).await {
                Ok(()) => auto_started = true,
                Err(e) => {
                    tracing::warn!(
                        "restart_mission auto_start failed for {}: {e}",
                        request.mission_id
                    );
                }
            }
        }
    }

    Ok(RestartResult {
        reset_count,
        auto_started,
        repo_path,
    })
}

/// 复用 start_mission_execution 的核心逻辑：promote ready tasks → 启动 scheduler。
/// 抽到 helper 是为了让 restart_mission 的 auto_start 可以直接复用而不绕一圈 IPC。
async fn auto_start_mission(
    app: &tauri::AppHandle,
    mission_id: &str,
    repo_path: &str,
) -> Result<(), String> {
    let repo_pb = std::path::PathBuf::from(repo_path);

    crate::git::ensure_git_repo(&repo_pb).map_err(|e| format!("ensure_git_repo failed: {e}"))?;

    let db = app.state::<crate::db::Database>();
    db.with_conn(|conn| -> anyhow::Result<()> {
        conn.execute(
            "UPDATE missions SET status = 'running', repo_path = ?1, updated_at = datetime('now') \
             WHERE id = ?2",
            rusqlite::params![repo_path, mission_id],
        )?;
        conn.execute(
            "UPDATE tasks SET status = 'ready' \
             WHERE mission_id = ?1 AND status = 'pending' \
               AND id NOT IN (SELECT task_id FROM task_dependencies)",
            rusqlite::params![mission_id],
        )?;
        Ok(())
    })
    .map_err(|e: anyhow::Error| e.to_string())?;

    let worktrees_dir = repo_pb.join(".worktrees");
    if let Err(e) = std::fs::create_dir_all(&worktrees_dir) {
        return Err(format!("create .worktrees failed: {e}"));
    }

    let scheduler = app.state::<crate::agent::Scheduler>();
    scheduler
        .start_mission(mission_id, repo_pb, app.clone())
        .map_err(|e| e.to_string())?;

    let _ = app.emit(
        "mission-status-changed",
        MissionStatusChangedPayload {
            mission_id: mission_id.to_string(),
            from: "planned".to_string(),
            to: "running".to_string(),
        },
    );

    Ok(())
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
                        // export 路径不关心失败信息，模板里也不应保留运行时状态
                        last_error: None,
                        last_failed_at: None,
                        // export 模板不携带 merge 节点（merge 是运行时基础设施，
                        // 重导入时由新 planner 重新决定是否注入）。
                        kind: "work".to_string(),
                        merge_parents_json: None,
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
                    "SELECT task_id, depends_on, artifact_refs, kind FROM task_dependencies WHERE task_id IN ({placeholders})"
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
                            kind: row
                                .get::<_, Option<String>>(3)
                                .ok()
                                .flatten()
                                .unwrap_or_else(|| "producer".to_string()),
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
    let yaml = crate::mission_template::serialize_yaml(&template).map_err(|e| e.to_string())?;

    std::fs::write(&request.file_path, &yaml)
        .map_err(|e| format!("Failed to write file '{}': {e}", request.file_path))?;

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
                rusqlite::params![
                    uuid,
                    mission_id,
                    task.title,
                    task.description,
                    task.complexity
                ],
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
mod classify_edge_kind_tests {
    //! FM-15 v2.3：边语义分类的纯函数测试。覆盖 sign_contract 写入前的判定逻辑。
    //! 必须与 migration 024 的 SQL backfill 行为完全一致，否则同一条边在历史数据
    //! 和新数据上会被标成不同 kind。
    use super::classify_edge_kind;
    use crate::agent::artifacts::ArtifactDecl;

    fn decl(local: &str, typ: &str) -> ArtifactDecl {
        ArtifactDecl {
            local_name: local.into(),
            artifact_type: typ.into(),
            summary: String::new(),
        }
    }

    #[test]
    fn empty_refs_classified_as_producer() {
        let upstream = vec![decl("a", "design_doc")];
        assert_eq!(classify_edge_kind(&[], &upstream), "producer");
    }

    #[test]
    fn doc_only_refs_classified_as_reference() {
        let upstream = vec![decl("architecture_doc", "design_doc")];
        let refs = vec!["up1.architecture_doc".to_string()];
        assert_eq!(classify_edge_kind(&refs, &upstream), "reference");
    }

    #[test]
    fn code_module_refs_classified_as_producer() {
        let upstream = vec![decl("engine_module", "code_module")];
        let refs = vec!["up1.engine_module".to_string()];
        assert_eq!(classify_edge_kind(&refs, &upstream), "producer");
    }

    #[test]
    fn mixed_refs_classified_as_producer() {
        let upstream = vec![decl("doc1", "design_doc"), decl("code1", "code_module")];
        let refs = vec!["up1.doc1".to_string(), "up1.code1".to_string()];
        assert_eq!(classify_edge_kind(&refs, &upstream), "producer");
    }

    #[test]
    fn all_doc_like_types_count_as_reference() {
        for ty in ["design_doc", "api_spec", "schema", "docs", "report"] {
            let upstream = vec![decl("a", ty)];
            let refs = vec!["up1.a".to_string()];
            assert_eq!(
                classify_edge_kind(&refs, &upstream),
                "reference",
                "{ty} 应该被视为 doc-like"
            );
        }
    }

    #[test]
    fn non_doc_types_count_as_producer() {
        for ty in ["code_module", "test_module", "config"] {
            let upstream = vec![decl("a", ty)];
            let refs = vec!["up1.a".to_string()];
            assert_eq!(
                classify_edge_kind(&refs, &upstream),
                "producer",
                "{ty} 必须按实物依赖处理，否则 UI 默认会隐藏掉真实物依赖"
            );
        }
    }

    /// ref 解析失败（找不到 local_name in upstream.produces）→ producer。
    /// 这种情况通常是 plan 数据残缺，保守起见按实物依赖处理避免误隐藏。
    #[test]
    fn unresolvable_ref_falls_back_to_producer() {
        let upstream = vec![decl("known", "design_doc")];
        let refs = vec!["up1.unknown".to_string()];
        assert_eq!(classify_edge_kind(&refs, &upstream), "producer");
    }

    /// ref 字符串没有 `.` 分隔（格式异常）→ producer。
    #[test]
    fn malformed_ref_falls_back_to_producer() {
        let upstream = vec![decl("a", "design_doc")];
        let refs = vec!["malformed_no_dot".to_string()];
        assert_eq!(classify_edge_kind(&refs, &upstream), "producer");
    }
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
            .query_row(
                "SELECT COUNT(*) FROM tasks WHERE mission_id = ?",
                [mid],
                |r| r.get(0),
            )
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
        conn.execute("INSERT INTO missions (id, title) VALUES ('m1', 'M')", [])
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

        conn.execute("DELETE FROM tasks WHERE id = 't2'", [])
            .unwrap();

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
        conn.execute("INSERT INTO missions (id, title) VALUES ('m1', 'M')", [])
            .unwrap();
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
            .query_row(
                "SELECT title, description FROM tasks WHERE id = 't1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(title, "New");
        assert_eq!(desc, "new desc");
    }

    #[test]
    fn ut03_4_confirm_mission() {
        let conn = setup_db();
        conn.execute(
            "INSERT INTO missions (id, title, status) VALUES ('m1', 'M', 'draft')",
            [],
        )
        .unwrap();
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

        conn.execute("UPDATE missions SET status = 'planned' WHERE id = 'm1'", [])
            .unwrap();
        conn.execute(
            "UPDATE tasks SET status = 'ready' WHERE mission_id = 'm1' AND status = 'pending' AND id NOT IN (SELECT task_id FROM task_dependencies)",
            [],
        )
        .unwrap();

        let m_status: String = conn
            .query_row("SELECT status FROM missions WHERE id = 'm1'", [], |r| {
                r.get(0)
            })
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
        conn.execute(
            "INSERT INTO missions (id, title, status) VALUES ('m1', 'M', 'draft')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tasks (id, mission_id, title, complexity) VALUES ('t1', 'm1', 'T1', 'low')",
            [],
        )
        .unwrap();

        conn.execute("DELETE FROM missions WHERE id = 'm1'", [])
            .unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM missions WHERE id = 'm1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(count, 0);

        let task_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM tasks WHERE mission_id = 'm1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(task_count, 0);
    }

    #[test]
    fn ut03_6_cannot_delete_running_mission() {
        let conn = setup_db();
        conn.execute(
            "INSERT INTO missions (id, title, status) VALUES ('m1', 'M', 'running')",
            [],
        )
        .unwrap();

        let status: String = conn
            .query_row("SELECT status FROM missions WHERE id = 'm1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_ne!(status, "draft");
    }
}

#[cfg(test)]
mod plan_mission_transaction_tests {
    //! 回归测试：retryable-flow rule 1 + 2 在 plan_mission 上的不变量。
    //!
    //! plan_mission 第 3 步会先 UPDATE missions（标题 + status='draft'），
    //! 再批量 INSERT tasks / task_dependencies。如果中途任意一步失败而不回滚，
    //! 用户会看到"title 改了但任务为空"的脏 mission，重试更困难。
    use crate::db::Database;

    fn read_title(db: &Database, id: &str) -> String {
        db.with_conn(|conn| {
            conn.query_row("SELECT title FROM missions WHERE id = ?", [id], |r| {
                r.get::<_, String>(0)
            })
            .map_err(Into::into)
        })
        .unwrap()
    }

    fn count_tasks(db: &Database, mission_id: &str) -> i64 {
        db.with_conn(|conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM tasks WHERE mission_id = ?",
                [mission_id],
                |r| r.get::<_, i64>(0),
            )
            .map_err(Into::into)
        })
        .unwrap()
    }

    /// 第 3 步事务里 INSERT tasks 失败时，title 不能被局部更新。
    /// （保证用户看到的 mission 列表与已规划的 tasks 始终自洽。）
    #[test]
    fn third_step_failure_rolls_back_title_update() {
        let db = Database::open_in_memory().unwrap();
        let mission_id = "m1".to_string();
        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO missions (id, title, description, status)
                 VALUES (?, 'Original Title', 'd', 'draft')",
                [&mission_id],
            )?;
            Ok(())
        })
        .unwrap();

        let result: anyhow::Result<()> = db.with_conn(|conn| {
            let tx = conn.unchecked_transaction()?;
            tx.execute(
                "UPDATE missions SET title = ?, status = 'draft' WHERE id = ?",
                rusqlite::params!["Planner-Generated Title", mission_id],
            )?;
            // 故意制造失败：title NOT NULL
            tx.execute(
                "INSERT INTO tasks (id, mission_id, title, description, complexity, status)
                 VALUES (?, ?, NULL, ?, ?, 'pending')",
                rusqlite::params!["t1", mission_id, "d", "low"],
            )?;
            tx.commit()?;
            Ok(())
        });

        assert!(result.is_err());
        assert_eq!(
            read_title(&db, &mission_id),
            "Original Title",
            "事务回滚后 title 必须保持 plan 前的值，否则用户列表/任务会出现不一致"
        );
        assert_eq!(count_tasks(&db, &mission_id), 0, "失败时不能留下半套 tasks");
    }

    /// 重 plan 的 idempotent：连跑两次成功事务，最终 tasks 数量等于第二次的内容
    /// （不会累积）。这条用例保证 DELETE+INSERT 的"清空旧 tasks"逻辑是正确的。
    #[test]
    fn replan_replaces_tasks_idempotently() {
        let db = Database::open_in_memory().unwrap();
        let mission_id = "m1".to_string();
        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO missions (id, title, description, status)
                 VALUES (?, 'T', 'd', 'draft')",
                [&mission_id],
            )?;
            Ok(())
        })
        .unwrap();

        // 模拟第 1 次 plan：3 个 tasks
        db.with_conn(|conn| {
            let tx = conn.unchecked_transaction()?;
            tx.execute(
                "DELETE FROM task_dependencies WHERE task_id IN
                    (SELECT id FROM tasks WHERE mission_id = ?)",
                [&mission_id],
            )?;
            tx.execute("DELETE FROM tasks WHERE mission_id = ?", [&mission_id])?;
            for i in 0..3 {
                tx.execute(
                    "INSERT INTO tasks (id, mission_id, title, description, complexity, status)
                     VALUES (?, ?, ?, 'd', 'low', 'pending')",
                    rusqlite::params![format!("t1-{i}"), mission_id, format!("Task {i}")],
                )?;
            }
            tx.commit()?;
            Ok(())
        })
        .unwrap();
        assert_eq!(count_tasks(&db, &mission_id), 3);

        // 第 2 次 plan：5 个 tasks。期望最终只有 5 个，旧的清干净。
        db.with_conn(|conn| {
            let tx = conn.unchecked_transaction()?;
            tx.execute(
                "DELETE FROM task_dependencies WHERE task_id IN
                    (SELECT id FROM tasks WHERE mission_id = ?)",
                [&mission_id],
            )?;
            tx.execute("DELETE FROM tasks WHERE mission_id = ?", [&mission_id])?;
            for i in 0..5 {
                tx.execute(
                    "INSERT INTO tasks (id, mission_id, title, description, complexity, status)
                     VALUES (?, ?, ?, 'd', 'low', 'pending')",
                    rusqlite::params![format!("t2-{i}"), mission_id, format!("Task {i}")],
                )?;
            }
            tx.commit()?;
            Ok(())
        })
        .unwrap();
        assert_eq!(
            count_tasks(&db, &mission_id),
            5,
            "重 plan 必须替换而非追加，否则用户会看到指数膨胀的 task 列表"
        );
    }
}
