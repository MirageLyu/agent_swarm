use anyhow::Result;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::{Emitter, Manager};
use tokio::time::{interval, Duration};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::agent::conflict_resolver::LlmProviderResolver;
use crate::agent::engine::AgentRunOptions;
use crate::agent::guardrail::{parse_guardrails, Guardrail};
use crate::agent::{AgentEngine, AgentRegistry, AgentStatus, EvaluatorAgent};
use crate::commands::{build_provider, ConfigManager};
use crate::db::queries;
use crate::db::Database;
use crate::git::llm_merge::{collect_conflict_blobs, ConflictBlob, LlmConflictResolver};
use crate::git::{MergeLayer, MergeStrategy, WorktreeManager};

const POLL_INTERVAL_MS: u64 = 1000;

// ---- Event payloads ----

#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentStartedPayload {
    pub agent_id: String,
    pub task_id: String,
    pub worktree_path: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct TaskStatusChangedPayload {
    pub task_id: String,
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct MissionStatusChangedPayload {
    pub mission_id: String,
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct MissionMergeProgressPayload {
    pub mission_id: String,
    pub branch: String,
    pub status: String, // "merged" | "conflict" | "skipped"
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct MissionMergeCompletedPayload {
    pub mission_id: String,
    pub total_merged: u32,
    pub errors: Vec<String>,
    pub auto_resolved: Vec<String>,
    /// FM-15 FR-08.2 (3): LLM 解决的文件清单
    pub llm_resolved: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskBasePreparedPayload {
    pub mission_id: String,
    pub task_id: String,
    pub base_branch: String,
    pub parent_count: u32,
    pub conflict_count: u32,
    /// 整体最高层："auto" | "heuristic_theirs" | "llm_failed_fallback"
    pub layer_summary: String,
}

// ---- Scheduler ----

pub struct Scheduler {
    missions: Mutex<HashMap<String, CancellationToken>>,
}

impl Scheduler {
    pub fn new() -> Self {
        Self {
            missions: Mutex::new(HashMap::new()),
        }
    }

    pub fn start_mission(
        &self,
        mission_id: &str,
        repo_path: PathBuf,
        app: tauri::AppHandle,
    ) -> Result<()> {
        let mut missions = self.missions.lock().unwrap();
        if missions.contains_key(mission_id) {
            anyhow::bail!("Mission {} is already being scheduled", mission_id);
        }
        let cancel = CancellationToken::new();
        missions.insert(mission_id.to_string(), cancel.clone());
        drop(missions);

        // FM-15 Phase 2 (FR-12): 启动时探测主分支并缓存到 mission 行；
        // 探测失败不阻塞启动（落到 fallback "main"，merge 阶段会再尝试），
        // 但记一条 warn 让用户在日志里能看到。
        if let Err(e) = Self::ensure_mission_main_branch(mission_id, &repo_path, &app) {
            tracing::warn!(
                "Scheduler: failed to detect main branch for mission {mission_id}: {e}; falling back to 'main'"
            );
        }

        let mid = mission_id.to_string();
        tokio::spawn(async move {
            Self::mission_loop(mid, repo_path, app, cancel).await;
        });

        Ok(())
    }

    /// FM-15 Phase 2 (FR-12): 若 mission 还没缓存主分支名，跑一次探测并写库。
    /// 已有缓存值则直接复用（避免每次启动都打开 repo 探测）。
    fn ensure_mission_main_branch(
        mission_id: &str,
        repo_path: &PathBuf,
        app: &tauri::AppHandle,
    ) -> Result<String> {
        let db = app.state::<Database>();
        if let Some(existing) = db.with_conn(|conn| queries::get_mission_main_branch(conn, mission_id))? {
            tracing::debug!(
                "Scheduler: mission {mission_id} main branch from cache = {existing}"
            );
            return Ok(existing);
        }
        let manager = WorktreeManager::new(repo_path.clone());
        let detected = manager.detect_main_branch()?;
        db.with_conn(|conn| queries::set_mission_main_branch(conn, mission_id, &detected))?;
        tracing::info!(
            "Scheduler: detected main branch '{detected}' for mission {mission_id}"
        );
        Ok(detected)
    }

    pub fn is_mission_active(&self, mission_id: &str) -> bool {
        self.missions.lock().unwrap().contains_key(mission_id)
    }

    pub fn stop_mission(&self, mission_id: &str) {
        if let Some(token) = self.missions.lock().unwrap().remove(mission_id) {
            token.cancel();
        }
    }

    pub fn active_count(&self) -> usize {
        self.missions.lock().unwrap().len()
    }

    // ---- Internal: mission polling loop ----

    async fn mission_loop(
        mission_id: String,
        repo_path: PathBuf,
        app: tauri::AppHandle,
        cancel: CancellationToken,
    ) {
        tracing::info!("Scheduler: starting loop for mission {mission_id}");
        let mut tick = interval(Duration::from_millis(POLL_INTERVAL_MS));

        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    tracing::info!("Scheduler: loop cancelled for mission {mission_id}");
                    break;
                }
                _ = tick.tick() => {
                    match Self::poll_and_dispatch(&mission_id, &repo_path, &app) {
                        Ok(true) => {
                            tracing::info!("Scheduler: mission {mission_id} reached terminal state");
                            break;
                        }
                        Err(e) => {
                            tracing::error!("Scheduler tick error for {mission_id}: {e}");
                        }
                        _ => {}
                    }
                }
            }
        }

        // Remove from active missions
        if let Some(scheduler) = app.try_state::<Scheduler>() {
            scheduler.missions.lock().unwrap().remove(&mission_id);
        }
        tracing::info!("Scheduler: loop ended for mission {mission_id}");
    }

    fn poll_and_dispatch(
        mission_id: &str,
        repo_path: &PathBuf,
        app: &tauri::AppHandle,
    ) -> Result<bool> {
        let db = app.state::<Database>();
        let config_mgr = app.state::<ConfigManager>();
        let config = config_mgr.get_config_snapshot();
        let max_concurrent = config.max_concurrent_agents as i64;

        // FM-15 Phase 2 (FR-13): 槽位按 mission 维度独立计算，多个 mission 不再共享配额。
        let running_count =
            db.with_conn(|conn| queries::count_running_agents_for_mission(conn, mission_id))?;

        let slots = max_concurrent - running_count;
        if slots <= 0 {
            // Still check terminal in case all running agents just finished
            let terminal = db.with_conn(|conn| queries::check_mission_terminal(conn, mission_id))?;
            return Ok(terminal.is_some());
        }

        let ready_tasks =
            db.with_conn(|conn| queries::get_ready_tasks_for_mission(conn, mission_id, slots))?;

        for task in &ready_tasks {
            if let Err(e) =
                Self::dispatch_task(mission_id, &task.id, &task.title, &task.description, repo_path, app)
            {
                tracing::error!("Failed to dispatch task {}: {e}", task.id);
            }
        }

        let terminal = db.with_conn(|conn| queries::check_mission_terminal(conn, mission_id))?;
        if let Some(ref new_status) = terminal {
            if new_status == "completed" {
                // FM-15 FR-08.2 (3): merge 路径含 async LLM 解冲突，必须在 tokio task 里跑。
                // 这里 spawn 不阻塞 poll loop 的退出。
                let mid = mission_id.to_string();
                let rp = repo_path.clone();
                let app_clone = app.clone();
                tokio::spawn(async move {
                    Self::merge_completed_mission(mid, rp, app_clone).await;
                });
            }
            let _ = app.emit(
                "mission-status-changed",
                MissionStatusChangedPayload {
                    mission_id: mission_id.to_string(),
                    from: "running".to_string(),
                    to: new_status.clone(),
                },
            );
        }
        Ok(terminal.is_some())
    }

    /// FM-15 Phase 2 (FR-08): mission 终态合并 —— frontier merge。
    ///
    /// 因为 dispatch 阶段每个 task 已经从 task-base/<id>（含全部上游产物）派生 worktree，
    /// frontier task（无已完成 successor 的叶子）的 commit 已累计包含所有上游产物。
    /// 因此只需 merge frontier，不需要再逐 agent 拓扑序合并。
    ///
    /// 每次 merge 走 L1 → L2 → Fallback 三层策略，并把记录写入 `merge_records`。
    /// FM-15 FR-08.2 (3): 当 mission.merge_strategy = `llm_resolve` 时，对 FallbackTheirs 文件
    /// 调用 LLM 解冲突，落一个 follow-up commit；LLM 失败的文件保留 theirs 兜底。
    async fn merge_completed_mission(
        mission_id: String,
        repo_path: PathBuf,
        app: tauri::AppHandle,
    ) {
        let mission_id = mission_id.as_str();
        let repo_path = &repo_path;
        let app = &app;
        tracing::info!("Starting frontier merge for completed mission {mission_id}");
        let db = app.state::<Database>();

        // FM-15 Phase 2 (FR-08.1): 只 merge frontier 任务，避免重复合并。
        let frontier = match db
            .with_conn(|conn| queries::get_frontier_completed_tasks(conn, mission_id))
        {
            Ok(rows) => rows,
            Err(e) => {
                tracing::error!("Failed to query frontier tasks for merge: {e}");
                return;
            }
        };

        if frontier.is_empty() {
            tracing::info!("No frontier tasks to merge for mission {mission_id}");
            return;
        }

        let main_branch_cached = db
            .with_conn(|conn| queries::get_mission_main_branch(conn, mission_id))
            .unwrap_or(None);
        let _wt_manager = match main_branch_cached {
            Some(name) => WorktreeManager::with_main_branch(repo_path.clone(), name),
            None => WorktreeManager::new(repo_path.clone()),
        };

        // 读取 mission 的合并策略（用户可在 mission 级显式指定，否则默认 'theirs' 与历史一致）
        let strategy_str: String = db
            .with_conn(|conn| {
                let s: Option<String> = conn
                    .query_row(
                        "SELECT merge_strategy FROM missions WHERE id = ?1",
                        rusqlite::params![mission_id],
                        |row| row.get::<_, Option<String>>(0),
                    )
                    .ok()
                    .flatten();
                Ok(s.unwrap_or_else(|| "theirs".to_string()))
            })
            .unwrap_or_else(|_| "theirs".to_string());
        let strategy = MergeStrategy::from_str(&strategy_str);

        // 当前 mission 的 main_branch 名（用于 merge_records.target_branch）
        let target_branch = db
            .with_conn(|conn| queries::get_mission_main_branch(conn, mission_id))
            .ok()
            .flatten()
            .unwrap_or_else(|| "main".to_string());

        // FM-15 FR-08.2 (3): 仅在 LlmResolve 模式下尝试构造 LLM resolver；
        // 构造失败（无 API key 等）→ 后续 fallback 到 ref-only theirs。
        let llm_resolver: Option<std::sync::Arc<dyn LlmConflictResolver>> =
            if matches!(strategy, MergeStrategy::LlmResolve) {
                match build_provider(app) {
                    Ok((provider, model)) => {
                        Some(std::sync::Arc::new(LlmProviderResolver::new(provider, model))
                            as std::sync::Arc<dyn LlmConflictResolver>)
                    }
                    Err(e) => {
                        tracing::warn!(
                            "LLM resolver unavailable ({e}); falling back to ref-only theirs for FallbackTheirs files"
                        );
                        None
                    }
                }
            } else {
                None
            };

        let mut total_merged: u32 = 0;
        let mut all_auto_resolved: Vec<String> = Vec::new();
        let mut all_llm_resolved: Vec<String> = Vec::new();
        let mut errors: Vec<String> = Vec::new();

        for (task_id, agent_id, _completed_at) in &frontier {
            let branch_name = format!("agent/{agent_id}");

            // (a) merge 之前先抓取潜在冲突的三方内容（merge 之后会 up-to-date 抓不到）
            let pre_conflicts: Vec<ConflictBlob> = if llm_resolver.is_some() {
                let target = target_branch.clone();
                let source = branch_name.clone();
                let rp = repo_path.clone();
                tokio::task::spawn_blocking(move || {
                    let repo = git2::Repository::open(&rp).ok()?;
                    collect_conflict_blobs(&repo, &target, &source).ok()
                })
                .await
                .ok()
                .flatten()
                .unwrap_or_default()
            } else {
                Vec::new()
            };

            // (b) 同步 ref-only merge（落 theirs 兜底 commit）
            let merge_result = {
                let agent_id = agent_id.clone();
                let wt = WorktreeManager::with_main_branch(repo_path.clone(), target_branch.clone());
                tokio::task::spawn_blocking(move || {
                    wt.merge_agent_branch_with_strategy(&agent_id, strategy)
                })
                .await
            };

            match merge_result {
                Ok(Ok(outcome)) => {
                    let conflicted_paths: Vec<String> =
                        outcome.conflicts.iter().map(|c| c.path.clone()).collect();
                    let mut llm_resolved_paths: Vec<String> = Vec::new();
                    let mut final_strategy = outcome.layer_summary.as_resolution_str().to_string();

                    if conflicted_paths.is_empty() {
                        tracing::info!(
                            "Merged {branch_name} → {target_branch} ({})",
                            outcome.commit_hash
                        );
                    } else {
                        tracing::info!(
                            "Merged {branch_name} → {target_branch} ({}), conflicts: {} ({})",
                            outcome.commit_hash,
                            conflicted_paths.len(),
                            final_strategy,
                        );
                        all_auto_resolved.extend(conflicted_paths.clone());
                    }
                    total_merged += 1;

                    // (c) FR-08.2 (3): 对 FallbackTheirs 文件调用 LLM resolver，写一个 follow-up commit
                    let fallback_paths: std::collections::HashSet<String> = outcome
                        .conflicts
                        .iter()
                        .filter(|c| c.layer == MergeLayer::FallbackTheirs)
                        .map(|c| c.path.clone())
                        .collect();
                    let llm_blobs: Vec<ConflictBlob> = pre_conflicts
                        .iter()
                        .filter(|b| fallback_paths.contains(&b.path))
                        .cloned()
                        .collect();

                    if let Some(resolver) = llm_resolver.as_ref() {
                        if !llm_blobs.is_empty() {
                            let mut resolutions: std::collections::HashMap<String, String> =
                                std::collections::HashMap::new();
                            for blob in &llm_blobs {
                                match resolver.resolve(blob).await {
                                    Ok(content) => {
                                        resolutions.insert(blob.path.clone(), content);
                                    }
                                    Err(e) => {
                                        tracing::warn!(
                                            "LLM resolve failed for `{}`: {e}; keeping theirs",
                                            blob.path
                                        );
                                    }
                                }
                            }
                            if !resolutions.is_empty() {
                                let resolved_files: Vec<String> = resolutions.keys().cloned().collect();
                                let wt = WorktreeManager::with_main_branch(
                                    repo_path.clone(),
                                    target_branch.clone(),
                                );
                                let msg = format!(
                                    "fix: LLM-resolved merge conflicts on {} (from {})",
                                    resolved_files.join(", "),
                                    branch_name
                                );
                                let res = tokio::task::spawn_blocking(move || {
                                    wt.apply_llm_resolutions(&resolutions, &msg)
                                })
                                .await;
                                match res {
                                    Ok(Ok(commit_hash)) => {
                                        tracing::info!(
                                            "Applied LLM resolutions for {} files on {target_branch} (commit {commit_hash})",
                                            resolved_files.len()
                                        );
                                        llm_resolved_paths.extend(resolved_files.clone());
                                        all_llm_resolved.extend(resolved_files);
                                        final_strategy = "llm_resolved".to_string();
                                    }
                                    Ok(Err(e)) => {
                                        tracing::warn!(
                                            "Failed to apply LLM resolutions on {target_branch}: {e}"
                                        );
                                    }
                                    Err(e) => {
                                        tracing::warn!(
                                            "spawn_blocking apply_llm_resolutions panicked: {e}"
                                        );
                                    }
                                }
                            }
                        }
                    }

                    // 落库 merge_records
                    let conflicted_json = serde_json::to_string(&conflicted_paths)
                        .unwrap_or_else(|_| "[]".to_string());
                    let record_id = Uuid::new_v4().to_string();
                    // FM-15 FR-08.2 (3): llm_resolution_succeeded 三态：
                    //   - 没有真冲突 / 没启用 LLM → None
                    //   - 启用且至少解决 1 个文件 → Some(true)
                    //   - 启用但全部失败回退 → Some(false)
                    let llm_succeeded: Option<bool> = if llm_resolver.is_some() && !fallback_paths.is_empty() {
                        Some(!llm_resolved_paths.is_empty())
                    } else {
                        None
                    };
                    let _ = db.with_conn(|conn| {
                        queries::record_merge_attempt(
                            conn,
                            &record_id,
                            mission_id,
                            &branch_name,
                            &target_branch,
                            &strategy_str,
                            &final_strategy,
                            &conflicted_json,
                            llm_succeeded,
                            None,
                            None,
                        )
                    });

                    let _ = app.emit(
                        "mission-merge-progress",
                        MissionMergeProgressPayload {
                            mission_id: mission_id.to_string(),
                            branch: branch_name.clone(),
                            status: "merged".to_string(),
                        },
                    );

                    // 清理 agent worktree（task-base 分支保留——它是产物溯源的快照）
                    {
                        let aid = agent_id.clone();
                        let wt = WorktreeManager::with_main_branch(
                            repo_path.clone(),
                            target_branch.clone(),
                        );
                        let res = tokio::task::spawn_blocking(move || wt.remove_worktree(&aid)).await;
                        if let Err(e) = res {
                            tracing::warn!("spawn_blocking remove_worktree panicked: {e}");
                        } else if let Ok(Err(e)) = res {
                            tracing::warn!("Failed to clean up worktree for {agent_id}: {e}");
                        }
                    }
                    let _ = task_id;
                }
                Ok(Err(e)) => {
                    tracing::error!("Failed to merge {branch_name}: {e}");
                    errors.push(branch_name.clone());
                    let _ = app.emit(
                        "mission-merge-progress",
                        MissionMergeProgressPayload {
                            mission_id: mission_id.to_string(),
                            branch: branch_name,
                            status: "error".to_string(),
                        },
                    );
                }
                Err(e) => {
                    tracing::error!("merge spawn_blocking panicked for {branch_name}: {e}");
                    errors.push(branch_name.clone());
                }
            }
        }

        tracing::info!(
            "Mission {mission_id} frontier merge complete: {total_merged} merged, {} errors, {} auto-resolved, {} llm-resolved",
            errors.len(),
            all_auto_resolved.len(),
            all_llm_resolved.len(),
        );

        let final_errors = errors.clone();
        let final_auto = all_auto_resolved.clone();
        let final_llm = all_llm_resolved.clone();
        let _ = app.emit(
            "mission-merge-completed",
            MissionMergeCompletedPayload {
                mission_id: mission_id.to_string(),
                total_merged,
                errors,
                auto_resolved: all_auto_resolved,
                llm_resolved: all_llm_resolved,
            },
        );

        // FM-15 v2.2 P4-S4: 仅当真的有 task 合入 main 才发 mission-delivered
        if total_merged > 0 && final_errors.is_empty() {
            Self::emit_mission_delivered(
                mission_id,
                repo_path,
                &target_branch,
                &final_auto,
                &final_llm,
                app,
            )
            .await;
        }
    }

    /// FM-15 v2.2 P4-S4: 聚合 deliverables 并广播 mission-delivered 事件。
    async fn emit_mission_delivered(
        mission_id: &str,
        repo_path: &PathBuf,
        main_branch: &str,
        auto_resolved: &[String],
        llm_resolved: &[String],
        app: &tauri::AppHandle,
    ) {
        let db = app.state::<Database>();

        // 已完成 task 数
        let total_tasks = db
            .with_conn(|conn| {
                let n: i64 = conn.query_row(
                    "SELECT COUNT(*) FROM tasks WHERE mission_id = ?1 AND status = 'completed'",
                    rusqlite::params![mission_id],
                    |row| row.get(0),
                )?;
                Ok(n)
            })
            .unwrap_or(0) as u32;

        // 主分支当前 commit hash（用于诊断显示）
        let total_commits = {
            let rp = repo_path.clone();
            let main = main_branch.to_string();
            tokio::task::spawn_blocking(move || -> u32 {
                let repo = match git2::Repository::open(&rp) {
                    Ok(r) => r,
                    Err(_) => return 0,
                };
                let branch = match repo.find_branch(&main, git2::BranchType::Local) {
                    Ok(b) => b,
                    Err(_) => return 0,
                };
                let head = match branch.get().peel_to_commit() {
                    Ok(c) => c,
                    Err(_) => return 0,
                };
                let mut walk = match repo.revwalk() {
                    Ok(w) => w,
                    Err(_) => return 0,
                };
                if walk.push(head.id()).is_err() {
                    return 0;
                }
                walk.count() as u32
            })
            .await
            .unwrap_or(0)
        };

        // 所有已发布 artifact + 关联 task title
        #[derive(Debug, Clone, serde::Serialize)]
        #[serde(rename_all = "camelCase")]
        struct ArtifactSummary {
            task_id: String,
            task_title: String,
            local_name: String,
            artifact_type: String,
            file_paths: Vec<String>,
            #[serde(skip_serializing_if = "Option::is_none")]
            summary: Option<String>,
        }

        let artifacts: Vec<ArtifactSummary> = db
            .with_conn(|conn| {
                let mut stmt = conn.prepare(
                    "SELECT a.producer_task_id, t.title, a.local_name, a.type, a.file_paths, a.summary
                     FROM artifacts a
                     JOIN tasks t ON t.id = a.producer_task_id
                     WHERE a.mission_id = ?1 AND a.published = 1
                     ORDER BY t.created_at, a.local_name",
                )?;
                let rows = stmt
                    .query_map(rusqlite::params![mission_id], |row| {
                        let file_paths_json: String = row.get(4)?;
                        let summary: Option<String> = row.get(5)?;
                        let summary = summary.filter(|s| !s.is_empty());
                        let file_paths: Vec<String> =
                            serde_json::from_str(&file_paths_json).unwrap_or_default();
                        Ok(ArtifactSummary {
                            task_id: row.get(0)?,
                            task_title: row.get(1)?,
                            local_name: row.get(2)?,
                            artifact_type: row.get(3)?,
                            file_paths,
                            summary,
                        })
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            })
            .unwrap_or_default();

        #[derive(Debug, Clone, serde::Serialize)]
        #[serde(rename_all = "camelCase")]
        struct MissionDeliveredPayload {
            mission_id: String,
            repo_path: String,
            main_branch: String,
            total_tasks: u32,
            total_commits: u32,
            artifacts: Vec<ArtifactSummary>,
            llm_resolved_files: Vec<String>,
            auto_resolved_files: Vec<String>,
        }

        let payload = MissionDeliveredPayload {
            mission_id: mission_id.to_string(),
            repo_path: repo_path.to_string_lossy().to_string(),
            main_branch: main_branch.to_string(),
            total_tasks,
            total_commits,
            artifacts,
            llm_resolved_files: llm_resolved.to_vec(),
            auto_resolved_files: auto_resolved.to_vec(),
        };
        if let Err(e) = app.emit("mission-delivered", payload) {
            tracing::warn!("Failed to emit mission-delivered for {mission_id}: {e}");
        } else {
            tracing::info!("Emitted mission-delivered for {mission_id}");
        }
    }

    fn dispatch_task(
        mission_id: &str,
        task_id: &str,
        task_title: &str,
        task_description: &str,
        repo_path: &PathBuf,
        app: &tauri::AppHandle,
    ) -> Result<()> {
        let db = app.state::<Database>();

        let claimed = db.with_conn(|conn| queries::claim_task(conn, task_id))?;
        if !claimed {
            return Ok(());
        }

        let _ = app.emit(
            "task-status-changed",
            TaskStatusChangedPayload {
                task_id: task_id.to_string(),
                from: "ready".to_string(),
                to: "running".to_string(),
            },
        );

        let agent_id = Uuid::new_v4().to_string();
        let agent_name = format!("Agent {}", &agent_id[..8]);

        // FM-15 Phase 2 (FR-12): 用 mission 缓存的主分支构造 manager，避免每次 dispatch 重新探测。
        let main_branch_cached = db
            .with_conn(|conn| queries::get_mission_main_branch(conn, mission_id))
            .unwrap_or(None);
        let wt_manager = match main_branch_cached.clone() {
            Some(name) => WorktreeManager::with_main_branch(repo_path.clone(), name),
            None => WorktreeManager::new(repo_path.clone()),
        };

        // FM-15 Phase 2 (FR-07): 增量 worktree 主路径：
        //   - 查询当前 task 的已完成直接父任务（拓扑后序）
        //   - 调用 prepare_task_base 在主仓库 ref-only 合并产生 task-base/<task_id> 分支
        //   - 把每个冲突写入 task_base_conflicts 表
        //   - emit task-base-prepared 事件让前端可观测
        //   - 之后 agent worktree 从 task-base/<task_id> 派生
        // 任一步骤失败时降级为旧路径（直接从 main 派生），并记 warn 日志便于观察。
        //
        // mission.use_incremental_worktree = 0 时跳过整段，走旧逻辑——保留 escape hatch。
        let use_incremental = db
            .with_conn(|conn| queries::get_mission_use_incremental_worktree(conn, mission_id))
            .unwrap_or(true);

        let worktree_base_branch: Option<String> = if use_incremental {
            match Self::build_task_base(mission_id, task_id, &wt_manager, &db, app) {
                Ok(Some(branch)) => Some(branch),
                Ok(None) => None, // 没有已完成父任务（根任务）→ 从 main 派生即可
                Err(e) => {
                    tracing::warn!(
                        "Task base preparation failed for task {task_id}, falling back to main: {e}"
                    );
                    None
                }
            }
        } else {
            None
        };

        let create_result = match worktree_base_branch.as_deref() {
            Some(branch) => wt_manager.create_worktree_from_branch(&agent_id, branch),
            None => wt_manager.create_worktree(&agent_id),
        };

        let worktree_path = match create_result {
            Ok(p) => p,
            Err(e) => {
                tracing::error!("Worktree creation failed for agent {agent_id}: {e}");
                let reason = format!("worktree_error: {e}");
                let _ = db.with_conn(|conn| {
                    queries::fail_task(conn, task_id, "failed", Some(&reason))
                });
                let _ = app.emit(
                    "task-status-changed",
                    TaskStatusChangedPayload {
                        task_id: task_id.to_string(),
                        from: "running".to_string(),
                        to: "failed".to_string(),
                    },
                );
                return Ok(());
            }
        };

        let wt_str = worktree_path.to_string_lossy().to_string();
        db.with_conn(|conn| {
            queries::insert_agent_for_task(conn, &agent_id, &agent_name, task_id, &wt_str)
        })?;

        // Capture base commit hash for post-merge code review
        if let Ok(repo) = git2::Repository::open(repo_path) {
            if let Ok(base_hash) = repo.head().and_then(|h| h.peel_to_commit().map(|c| c.id().to_string())) {
                let _ = db.with_conn(|conn| queries::save_agent_base_commit(conn, &agent_id, &base_hash));
            }
        }

        let (provider, model) = build_provider(app).map_err(|e| anyhow::anyhow!(e))?;

        let registry = app.state::<AgentRegistry>();
        let cancel_token = registry.register(&agent_id);

        let engine = AgentEngine::new(provider, worktree_path, app.clone(), cancel_token);

        let _ = app.emit(
            "agent-started",
            AgentStartedPayload {
                agent_id: agent_id.clone(),
                task_id: task_id.to_string(),
                worktree_path: wt_str,
            },
        );

        let aid = agent_id;
        let tid = task_id.to_string();
        let task_title_owned = task_title.to_string();

        let directives = db
            .with_conn(|conn| queries::get_mission_directives(conn, mission_id))
            .unwrap_or_default();
        let task_desc = if directives.is_empty() {
            format!("{task_title}\n\n{task_description}")
        } else {
            format!(
                "{task_title}\n\n{task_description}\n\n\
                 [Standing Mission Directives — you MUST follow these]\n{directives}"
            )
        };

        // FM-15 Phase 3 (FR-09 / FR-11): 装载 task 上的 guardrails / produces / expected_output
        // + 全局 max_agent_steps / agent_timeout_seconds 配置，组装 AgentRunOptions。
        let agent_options =
            Self::build_agent_run_options(task_id, model.clone(), &db, app)
                .unwrap_or_else(|e| {
                    tracing::warn!(
                        "build_agent_run_options failed for {task_id}: {e}; falling back to defaults"
                    );
                    AgentRunOptions {
                        model: model.clone(),
                        ..AgentRunOptions::default()
                    }
                });

        let repo_path_owned = repo_path.clone();
        let app_clone = app.clone();

        tokio::spawn(async move {
            let result = engine
                .run_with_options(&aid, &task_desc, &agent_options)
                .await;

            // engine 内部已在 last_error 写好分类原因；scheduler 只在 Err（panic / 提前 bail）
            // 时补一条兜底原因，正常 AgentStatus::Failed 不覆盖。
            let mut scheduler_failure_reason: Option<String> = None;
            let task_status = match &result {
                Ok(AgentStatus::Completed) => "completed",
                Ok(AgentStatus::Cancelled) => "cancelled",
                Ok(_) | Err(_) => {
                    if let Err(e) = &result {
                        tracing::error!("Agent {aid} error: {e}");
                        let msg = format!("agent_error: {e}");
                        let db = app_clone.state::<Database>();
                        let _ = db.with_conn(|conn| {
                            conn.execute(
                                "UPDATE agents SET status = 'failed', \
                                 error_message = COALESCE(error_message, ?2), \
                                 updated_at = datetime('now') WHERE id = ?1",
                                rusqlite::params![aid, &msg],
                            )?;
                            Ok(())
                        });
                        scheduler_failure_reason = Some(msg);
                    } else {
                        let db = app_clone.state::<Database>();
                        let _ = db.with_conn(|conn| {
                            conn.execute(
                                "UPDATE agents SET status = 'failed', updated_at = datetime('now') WHERE id = ?1",
                                rusqlite::params![aid],
                            )?;
                            Ok(())
                        });
                    }
                    "failed"
                }
            };

            let db = app_clone.state::<Database>();

            let _ = db.with_conn(|conn| {
                if task_status == "completed" {
                    queries::complete_task(conn, &tid)
                } else {
                    queries::fail_task(conn, &tid, task_status, scheduler_failure_reason.as_deref())
                }
            });

            let _ = app_clone.emit(
                "task-status-changed",
                TaskStatusChangedPayload {
                    task_id: tid.clone(),
                    from: "running".to_string(),
                    to: task_status.to_string(),
                },
            );

            if task_status == "completed" {
                let wt_manager = WorktreeManager::new(repo_path_owned.clone());
                let commit_msg = format!("[Task] {task_title_owned}");
                match wt_manager.commit_worktree(&aid, &commit_msg) {
                    Ok(Some(hash)) => {
                        tracing::info!("Agent {aid} work committed: {hash}");
                        let _ = db.with_conn(|conn| queries::save_agent_head_commit(conn, &aid, &hash));
                    }
                    Ok(None) => {
                        tracing::info!("Agent {aid} produced no file changes to commit");
                    }
                    Err(e) => {
                        tracing::error!("Failed to commit agent {aid} work: {e}");
                    }
                }

                if let Ok(promoted) =
                    db.with_conn(|conn| queries::advance_dependencies(conn, &tid))
                {
                    for promoted_id in promoted {
                        let _ = app_clone.emit(
                            "task-status-changed",
                            TaskStatusChangedPayload {
                                task_id: promoted_id,
                                from: "pending".to_string(),
                                to: "ready".to_string(),
                            },
                        );
                    }
                }

                // FM-11: Trigger Evaluator Agent (runs in background, doesn't block)
                Self::spawn_evaluator(&aid, &repo_path_owned, &app_clone);
            }

            // Terminal detection + merge is handled exclusively by poll_and_dispatch
            // to avoid race conditions with duplicate merge calls.

            let registry = app_clone.state::<AgentRegistry>();
            registry.remove(&aid);
        });

        Ok(())
    }

    /// FM-15 Phase 3 (FR-09 / FR-11): 从 DB + 配置组装 AgentRunOptions。
    ///
    /// 读取顺序：
    /// 1. tasks.guardrails / guardrail_retry_budget / produces_artifacts / expected_output
    /// 2. config.max_agent_steps / config.agent_timeout_seconds
    fn build_agent_run_options(
        task_id: &str,
        model: String,
        db: &Database,
        app: &tauri::AppHandle,
    ) -> Result<AgentRunOptions> {
        let cfg = app.state::<ConfigManager>().get_config_snapshot();
        let row: (String, i64, String, Option<String>) = db.with_conn(|conn| {
            let row = conn
                .query_row(
                    "SELECT guardrails, guardrail_retry_budget, produces_artifacts, expected_output \
                     FROM tasks WHERE id = ?1",
                    rusqlite::params![task_id],
                    |row| {
                        Ok((
                            row.get::<_, Option<String>>(0)?.unwrap_or_else(|| "[]".to_string()),
                            row.get::<_, i64>(1)?,
                            row.get::<_, Option<String>>(2)?.unwrap_or_else(|| "[]".to_string()),
                            row.get::<_, Option<String>>(3)?,
                        ))
                    },
                )?;
            Ok(row)
        })?;
        let (guardrails_json, retry_budget, produces_json, expected_output) = row;
        let guardrails: Vec<Guardrail> = parse_guardrails(&guardrails_json);

        // produces_artifacts JSON → Vec<(local_name, type)>
        let produces: Vec<(String, String)> = serde_json::from_str::<serde_json::Value>(&produces_json)
            .ok()
            .and_then(|v| {
                v.as_array().map(|arr| {
                    arr.iter()
                        .filter_map(|item| {
                            let name = item.get("local_name").and_then(|v| v.as_str())?;
                            let ty = item.get("type").and_then(|v| v.as_str())?;
                            Some((name.to_string(), ty.to_string()))
                        })
                        .collect()
                })
            })
            .unwrap_or_default();

        Ok(AgentRunOptions {
            model,
            max_steps: cfg.max_agent_steps,
            timeout_secs: cfg.agent_timeout_seconds,
            guardrails,
            guardrail_retry_budget: retry_budget.max(0) as u32,
            produces,
            expected_output: expected_output.filter(|s| !s.trim().is_empty()),
        })
    }

    /// FM-15 Phase 2 (FR-07.1): 为 task 准备增量 base 分支并写入冲突记录。
    ///
    /// 返回值含义：
    /// - `Ok(Some(branch))`：成功，agent worktree 应基于该 task-base 分支派生
    /// - `Ok(None)`：该 task 没有已完成的父任务（根任务），无需 base 准备，agent 直接从 main 派生
    /// - `Err(_)`：准备失败（git 操作出错），调用方降级为从 main 派生
    fn build_task_base(
        mission_id: &str,
        task_id: &str,
        wt_manager: &WorktreeManager,
        db: &Database,
        app: &tauri::AppHandle,
    ) -> Result<Option<String>> {
        let parents = db.with_conn(|conn| queries::get_completed_parent_tasks_for(conn, task_id))?;
        if parents.is_empty() {
            return Ok(None);
        }

        // 读取 mission 的合并策略（默认 'theirs'，与历史行为一致；'llm_resolve' 在 Phase 3 接 LLM）
        let strategy_str: String = db.with_conn(|conn| {
            let s: Option<String> = conn
                .query_row(
                    "SELECT merge_strategy FROM missions WHERE id = ?1",
                    rusqlite::params![mission_id],
                    |row| row.get::<_, Option<String>>(0),
                )
                .ok()
                .flatten();
            Ok(s.unwrap_or_else(|| "theirs".to_string()))
        })?;
        let strategy = MergeStrategy::from_str(&strategy_str);

        let outcome = wt_manager.prepare_task_base(task_id, &parents, strategy)?;

        // 把每条冲突落库（resolution 字段直接来自 MergeLayer）
        if !outcome.conflicts.is_empty() {
            db.with_conn(|conn| {
                for c in outcome.conflicts.iter() {
                    queries::record_task_base_conflict(
                        conn,
                        task_id,
                        &c.parent_task_id,
                        &c.file_path,
                        c.layer.as_resolution_str(),
                    )?;
                }
                Ok(())
            })?;
        }

        let _ = app.emit(
            "task-base-prepared",
            TaskBasePreparedPayload {
                mission_id: mission_id.to_string(),
                task_id: task_id.to_string(),
                base_branch: outcome.base_branch.clone(),
                parent_count: outcome.parent_summaries.len() as u32,
                conflict_count: outcome.conflicts.len() as u32,
                layer_summary: outcome.layer_summary.as_resolution_str().to_string(),
            },
        );

        tracing::info!(
            "Task {task_id} base ready on '{}' ({} parents, {} conflicts, layer={})",
            outcome.base_branch,
            outcome.parent_summaries.len(),
            outcome.conflicts.len(),
            outcome.layer_summary.as_resolution_str(),
        );

        Ok(Some(outcome.base_branch))
    }

    /// Spawn an Evaluator Agent in the background for a completed coding agent.
    /// Evaluators don't count against max_concurrent_agents (NFR).
    /// Timeout: 30s (BT-05). Duplicate prevention: BT-07.
    fn spawn_evaluator(
        agent_id: &str,
        repo_path: &PathBuf,
        app: &tauri::AppHandle,
    ) {
        let (provider, model) = match build_provider(app) {
            Ok(pm) => pm,
            Err(e) => {
                tracing::error!("Evaluator: cannot build provider for agent {agent_id}: {e}");
                return;
            }
        };

        let db = app.state::<Database>();
        let mission_id = match db.with_conn(|conn| queries::get_mission_id_for_agent(conn, agent_id)) {
            Ok(Some(mid)) => mid,
            _ => {
                tracing::warn!("Evaluator: cannot find mission for agent {agent_id}, skipping");
                return;
            }
        };

        let aid = agent_id.to_string();
        let rp = repo_path.clone();
        let app_clone = app.clone();

        tokio::spawn(async move {
            let evaluator = EvaluatorAgent::new(provider, model, app_clone);
            let result = tokio::time::timeout(
                tokio::time::Duration::from_secs(30),
                evaluator.evaluate(&aid, &mission_id, &rp),
            )
            .await;

            match result {
                Ok(Ok(())) => {
                    tracing::info!("Evaluator: finished for agent {aid}");
                }
                Ok(Err(e)) => {
                    tracing::error!("Evaluator: failed for agent {aid}: {e}");
                }
                Err(_) => {
                    tracing::warn!("Evaluator: timed out for agent {aid} (BT-05)");
                }
            }
        });
    }
}
