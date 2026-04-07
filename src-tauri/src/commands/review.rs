use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tauri::Manager;
use uuid::Uuid;

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
