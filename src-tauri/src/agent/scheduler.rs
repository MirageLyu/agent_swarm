use anyhow::Result;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::{Emitter, Manager};
use tokio::time::{interval, Duration};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::agent::{AgentEngine, AgentRegistry, AgentStatus};
use crate::commands::{build_provider, ConfigManager};
use crate::db::queries;
use crate::db::Database;
use crate::git::{WorktreeManager, MergeOutcome};

const POLL_INTERVAL_MS: u64 = 1000;
const MAX_AGENT_STEPS: u32 = u32::MAX;

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

        let mid = mission_id.to_string();
        tokio::spawn(async move {
            Self::mission_loop(mid, repo_path, app, cancel).await;
        });

        Ok(())
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

        let running_count = db.with_conn(|conn| queries::count_running_agents(conn))?;

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
                Self::merge_completed_mission(mission_id, repo_path, app);
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

    fn merge_completed_mission(
        mission_id: &str,
        repo_path: &PathBuf,
        app: &tauri::AppHandle,
    ) {
        tracing::info!("Starting merge for completed mission {mission_id}");
        let db = app.state::<Database>();

        let agent_ids = match db.with_conn(|conn| {
            queries::get_completed_agents_topo_order(conn, mission_id)
        }) {
            Ok(ids) => ids,
            Err(e) => {
                tracing::error!("Failed to query agents for merge: {e}");
                return;
            }
        };

        if agent_ids.is_empty() {
            tracing::info!("No agent branches to merge for mission {mission_id}");
            return;
        }

        let wt_manager = WorktreeManager::new(repo_path.clone());
        let mut total_merged: u32 = 0;
        let mut all_auto_resolved: Vec<String> = Vec::new();
        let mut errors: Vec<String> = Vec::new();

        for aid in &agent_ids {
            let branch_name = format!("agent/{aid}");
            match wt_manager.merge_agent_branch(aid) {
                Ok(MergeOutcome::Merged { commit_hash, auto_resolved }) => {
                    if auto_resolved.is_empty() {
                        tracing::info!("Merged {branch_name} → main ({commit_hash})");
                    } else {
                        tracing::info!(
                            "Merged {branch_name} → main ({commit_hash}), auto-resolved: {}",
                            auto_resolved.join(", ")
                        );
                        all_auto_resolved.extend(auto_resolved);
                    }
                    total_merged += 1;
                    let _ = app.emit(
                        "mission-merge-progress",
                        MissionMergeProgressPayload {
                            mission_id: mission_id.to_string(),
                            branch: branch_name.clone(),
                            status: "merged".to_string(),
                        },
                    );
                    if let Err(e) = wt_manager.remove_worktree(aid) {
                        tracing::warn!("Failed to clean up worktree for {aid}: {e}");
                    }
                }
                Err(e) => {
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
            }
        }

        tracing::info!(
            "Mission {mission_id} merge complete: {total_merged} merged, {} errors, {} auto-resolved files",
            errors.len(),
            all_auto_resolved.len()
        );

        let _ = app.emit(
            "mission-merge-completed",
            MissionMergeCompletedPayload {
                mission_id: mission_id.to_string(),
                total_merged,
                errors,
                auto_resolved: all_auto_resolved,
            },
        );
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

        // Create worktree
        let wt_manager = WorktreeManager::new(repo_path.clone());
        let worktree_path = match wt_manager.create_worktree(&agent_id) {
            Ok(p) => p,
            Err(e) => {
                tracing::error!("Worktree creation failed for agent {agent_id}: {e}");
                let _ = db.with_conn(|conn| queries::fail_task(conn, task_id, "failed"));
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
        let mid = mission_id.to_string();
        let task_title_owned = task_title.to_string();
        let task_desc = format!("{task_title}\n\n{task_description}");
        let repo_path_owned = repo_path.clone();
        let app_clone = app.clone();

        tokio::spawn(async move {
            let result = engine.run(&aid, &task_desc, &model, MAX_AGENT_STEPS).await;

            let task_status = match &result {
                Ok(AgentStatus::Completed) => "completed",
                Ok(AgentStatus::Cancelled) => "cancelled",
                Ok(_) | Err(_) => {
                    if let Err(e) = &result {
                        tracing::error!("Agent {aid} error: {e}");
                    }
                    let db = app_clone.state::<Database>();
                    let _ = db.with_conn(|conn| {
                        conn.execute(
                            "UPDATE agents SET status = 'failed', updated_at = datetime('now') WHERE id = ?1",
                            rusqlite::params![aid],
                        )?;
                        Ok(())
                    });
                    "failed"
                }
            };

            let db = app_clone.state::<Database>();

            let _ = db.with_conn(|conn| {
                if task_status == "completed" {
                    queries::complete_task(conn, &tid)
                } else {
                    queries::fail_task(conn, &tid, task_status)
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
                // Auto-commit agent's work in the worktree
                let wt_manager = WorktreeManager::new(repo_path_owned.clone());
                let commit_msg = format!("[Task] {task_title_owned}");
                match wt_manager.commit_worktree(&aid, &commit_msg) {
                    Ok(Some(hash)) => {
                        tracing::info!("Agent {aid} work committed: {hash}");
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
            }

            if let Ok(Some(new_status)) =
                db.with_conn(|conn| queries::check_mission_terminal(conn, &mid))
            {
                if new_status == "completed" {
                    Self::merge_completed_mission(&mid, &repo_path_owned, &app_clone);
                }
                let _ = app_clone.emit(
                    "mission-status-changed",
                    MissionStatusChangedPayload {
                        mission_id: mid.clone(),
                        from: "running".to_string(),
                        to: new_status,
                    },
                );
            }

            let registry = app_clone.state::<AgentRegistry>();
            registry.remove(&aid);
        });

        Ok(())
    }
}
