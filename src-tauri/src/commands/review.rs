use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tauri::Manager;
use uuid::Uuid;

use crate::agent::EvaluatorAgent;
use crate::commands::build_provider;
use crate::db::{queries, Database};
use crate::git::{DiffFile, WorktreeManager};

#[derive(Debug, Serialize, Clone)]
pub struct AgentDiffResponse {
    pub agent_id: String,
    pub files: Vec<DiffFile>,
    pub review_status: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SubmitReviewActionRequest {
    pub agent_id: String,
    pub action: String,
    pub comment: Option<String>,
}

#[tauri::command]
pub fn get_agent_diff(
    app: tauri::AppHandle,
    agent_id: String,
) -> Result<AgentDiffResponse, String> {
    let db = app.state::<Database>();

    let worktree_path: Option<String> = db
        .with_conn(|conn| {
            conn.query_row(
                "SELECT worktree_path FROM agents WHERE id = ?1",
                [&agent_id],
                |row: &rusqlite::Row| row.get(0),
            )
            .map_err(|e| anyhow::anyhow!("Agent not found: {e}"))
        })
        .map_err(|e: anyhow::Error| e.to_string())?;

    let worktree_path =
        worktree_path.ok_or_else(|| "Agent has no worktree path".to_string())?;

    let wt_path = PathBuf::from(&worktree_path);
    let repo_path = wt_path
        .parent()
        .and_then(|p| p.parent())
        .ok_or_else(|| "Cannot determine repo path from worktree path".to_string())?;

    let wt_manager = WorktreeManager::new(repo_path.to_path_buf());

    // Try branch-based diff first; fall back to stored commit hashes
    // when the branch has been deleted after merge.
    let files = match wt_manager.get_structured_diff(&agent_id) {
        Ok(f) => f,
        Err(_) => {
            let hashes = db
                .with_conn(|conn| queries::get_agent_commit_hashes(conn, &agent_id))
                .map_err(|e: anyhow::Error| e.to_string())?;
            match (hashes.base_commit_hash, hashes.head_commit_hash) {
                (Some(base), Some(head)) => wt_manager
                    .get_structured_diff_by_hashes(&base, &head)
                    .map_err(|e| e.to_string())?,
                _ => {
                    return Err(
                        "Agent branch was removed after merge and no commit hashes were saved. \
                         Re-run the mission to generate reviewable diffs."
                            .to_string(),
                    );
                }
            }
        }
    };

    let review_status = db
        .with_conn(|conn| queries::get_latest_review_status(conn, &agent_id))
        .map_err(|e: anyhow::Error| e.to_string())?;

    Ok(AgentDiffResponse {
        agent_id,
        files,
        review_status,
    })
}

#[tauri::command]
pub fn submit_review_action(
    app: tauri::AppHandle,
    request: SubmitReviewActionRequest,
) -> Result<(), String> {
    let valid_actions = ["approved", "rejected", "revision_requested"];
    if !valid_actions.contains(&request.action.as_str()) {
        return Err(format!(
            "Invalid review action '{}'. Must be one of: {}",
            request.action,
            valid_actions.join(", ")
        ));
    }

    if request.action == "revision_requested" && request.comment.as_deref().unwrap_or("").trim().is_empty() {
        return Err("Request Revision requires a non-empty comment".to_string());
    }

    let event_id = Uuid::new_v4().to_string();
    let content = serde_json::json!({
        "action": request.action,
        "comment": request.comment.unwrap_or_default(),
    })
    .to_string();

    let db = app.state::<Database>();
    db.with_conn(|conn| {
        queries::insert_event(conn, &event_id, &request.agent_id, 0, "review", &content)
    })
    .map_err(|e: anyhow::Error| e.to_string())?;

    Ok(())
}

// ---- FM-11: Evaluator Agent commands ----

#[derive(Debug, Serialize, Clone)]
pub struct TriggerEvaluationResponse {
    pub evaluator_agent_id: String,
}

#[derive(Debug, Serialize, Clone)]
pub struct EvaluationResult {
    pub agent_id: String,
    pub overall_score: f64,
    pub summary: String,
    pub contract_compliance: Option<String>,
    pub annotation_count: u32,
    pub auto_fixed_count: u32,
    pub needs_review_count: u32,
    pub created_at: String,
}

#[derive(Debug, Serialize, Clone)]
pub struct AnnotationInfo {
    pub id: String,
    pub review_id: String,
    pub agent_id: String,
    pub file_path: String,
    pub line_number: i64,
    #[serde(rename = "type")]
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

#[derive(Debug, Deserialize)]
pub struct GetAnnotationsRequest {
    pub agent_id: String,
    pub file_path: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateAnnotationStatusRequest {
    pub annotation_id: String,
    pub status: String,
}

#[tauri::command]
pub async fn trigger_evaluation(
    app: tauri::AppHandle,
    agent_id: String,
) -> Result<TriggerEvaluationResponse, String> {
    let db = app.state::<Database>();

    // BT-07: Prevent duplicate evaluation
    let already = db
        .with_conn(|conn| queries::has_evaluator_review(conn, &agent_id))
        .map_err(|e| e.to_string())?;
    if already {
        return Err("Agent already has an evaluation review".to_string());
    }

    let mission_id = db
        .with_conn(|conn| queries::get_mission_id_for_agent(conn, &agent_id))
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "Cannot find mission for agent".to_string())?;

    let repo_path_str = db
        .with_conn(|conn| queries::get_mission_repo_path(conn, &mission_id))
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "Mission has no repo_path".to_string())?;
    let repo_path = PathBuf::from(&repo_path_str);

    let (provider, model) = build_provider(&app).map_err(|e| e.to_string())?;

    let evaluator = EvaluatorAgent::new(provider, model, app.clone());
    let eval_id = format!("eval-{}", &agent_id[..8.min(agent_id.len())]);

    let aid = agent_id.clone();
    let mid = mission_id.clone();
    tokio::spawn(async move {
        let result = tokio::time::timeout(
            tokio::time::Duration::from_secs(30),
            evaluator.evaluate(&aid, &mid, &repo_path),
        )
        .await;

        match result {
            Ok(Ok(())) => tracing::info!("Manual evaluator finished for agent {aid}"),
            Ok(Err(e)) => tracing::error!("Manual evaluator failed for agent {aid}: {e}"),
            Err(_) => tracing::warn!("Manual evaluator timed out for agent {aid}"),
        }
    });

    Ok(TriggerEvaluationResponse {
        evaluator_agent_id: eval_id,
    })
}

#[tauri::command]
pub fn get_evaluation_result(
    app: tauri::AppHandle,
    agent_id: String,
) -> Result<Option<EvaluationResult>, String> {
    let db = app.state::<Database>();

    let review = db
        .with_conn(|conn| queries::get_evaluator_review_for_agent(conn, &agent_id))
        .map_err(|e| e.to_string())?;

    match review {
        None => Ok(None),
        Some(r) => {
            let annotations = db
                .with_conn(|conn| queries::get_annotations_for_agent(conn, &agent_id, None))
                .map_err(|e| e.to_string())?;

            let auto_fixed_count = annotations.iter().filter(|a| a.status == "auto_fixed").count() as u32;
            let needs_review_count = annotations
                .iter()
                .filter(|a| a.status == "open" || a.status == "revision_requested")
                .count() as u32;

            Ok(Some(EvaluationResult {
                agent_id: r.agent_id,
                overall_score: r.overall_score,
                summary: r.summary,
                contract_compliance: r.contract_compliance,
                annotation_count: annotations.len() as u32,
                auto_fixed_count,
                needs_review_count,
                created_at: r.created_at,
            }))
        }
    }
}

#[tauri::command]
pub fn get_annotations(
    app: tauri::AppHandle,
    request: GetAnnotationsRequest,
) -> Result<Vec<AnnotationInfo>, String> {
    let db = app.state::<Database>();
    let rows = db
        .with_conn(|conn| {
            queries::get_annotations_for_agent(conn, &request.agent_id, request.file_path.as_deref())
        })
        .map_err(|e| e.to_string())?;

    Ok(rows
        .into_iter()
        .map(|r| AnnotationInfo {
            id: r.id,
            review_id: r.review_id,
            agent_id: r.agent_id,
            file_path: r.file_path,
            line_number: r.line_number,
            ann_type: r.ann_type,
            severity: r.severity,
            status: r.status,
            message: r.message,
            suggestion: r.suggestion,
            auto_fixable: r.auto_fixable,
            original_code: r.original_code,
            fixed_code: r.fixed_code,
            created_at: r.created_at,
        })
        .collect())
}

#[tauri::command]
pub fn update_annotation_status(
    app: tauri::AppHandle,
    request: UpdateAnnotationStatusRequest,
) -> Result<(), String> {
    let valid_statuses = ["open", "auto_fixed", "revision_requested", "dismissed"];
    if !valid_statuses.contains(&request.status.as_str()) {
        return Err(format!(
            "Invalid annotation status '{}'. Must be one of: {}",
            request.status,
            valid_statuses.join(", ")
        ));
    }

    let db = app.state::<Database>();
    let updated = db
        .with_conn(|conn| queries::update_annotation_status(conn, &request.annotation_id, &request.status))
        .map_err(|e| e.to_string())?;

    if !updated {
        return Err("Annotation not found".to_string());
    }

    Ok(())
}
