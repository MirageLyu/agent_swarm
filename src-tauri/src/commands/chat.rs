//! FM-15 v2.2 P4-S5: Chat / Follow-up IPC 命令。

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tauri::Manager;
use uuid::Uuid;

use crate::agent::chat::{ChatAgent, ChatTurnSummary};
use crate::commands::mission::build_provider;
use crate::db::queries;
use crate::git::WorktreeManager;

#[derive(Debug, Deserialize)]
pub struct SendChatMessageRequest {
    pub mission_id: String,
    pub content: String,
    /// FR-15.4 用户拒绝升级后用 force_direct = true 强制 chat 直接做。
    #[serde(default)]
    pub force_direct: bool,
}

#[derive(Debug, Serialize)]
pub struct ChatMessageInfo {
    pub id: String,
    pub mission_id: String,
    pub role: String,
    pub content: String,
    pub tool_calls: Option<String>,
    pub artifact_refs: Option<String>,
    pub proposed_followup_mission_id: Option<String>,
    pub created_at: String,
}

impl From<queries::MissionChatRow> for ChatMessageInfo {
    fn from(row: queries::MissionChatRow) -> Self {
        Self {
            id: row.id,
            mission_id: row.mission_id,
            role: row.role,
            content: row.content,
            tool_calls: row.tool_calls,
            artifact_refs: row.artifact_refs,
            proposed_followup_mission_id: row.proposed_followup_mission_id,
            created_at: row.created_at,
        }
    }
}

#[tauri::command]
pub fn list_chat_messages(
    app: tauri::AppHandle,
    mission_id: String,
) -> Result<Vec<ChatMessageInfo>, String> {
    let db = app.state::<crate::db::Database>();
    let rows = db
        .with_conn(|c| queries::list_mission_chats(c, &mission_id))
        .map_err(|e| e.to_string())?;
    Ok(rows.into_iter().map(ChatMessageInfo::from).collect())
}

#[tauri::command]
pub async fn send_chat_message(
    app: tauri::AppHandle,
    request: SendChatMessageRequest,
) -> Result<ChatTurnSummary, String> {
    let SendChatMessageRequest { mission_id, content, force_direct } = request;
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return Err("Message content must not be empty".into());
    }

    let (provider, model) = build_provider(&app)?;

    // 获取 repo_path & main_branch
    let db = app.state::<crate::db::Database>();
    let (repo_path_opt, main_branch_opt, status): (
        Option<String>,
        Option<String>,
        String,
    ) = db
        .with_conn(|c| {
            c.query_row(
                "SELECT repo_path, main_branch, status FROM missions WHERE id = ?1",
                [&mission_id],
                |r| {
                    Ok((
                        r.get::<_, Option<String>>(0)?,
                        r.get::<_, Option<String>>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                },
            )
            .map_err(anyhow::Error::from)
        })
        .map_err(|e| format!("Mission not found: {e}"))?;

    if !matches!(status.as_str(), "completed" | "failed" | "running") {
        return Err(format!(
            "Chat is only available after the mission has produced output (current status: {status})"
        ));
    }

    let repo_path_str =
        repo_path_opt.ok_or_else(|| "Mission has no repo_path; cannot run chat agent".to_string())?;
    let repo_path = PathBuf::from(&repo_path_str);
    if !repo_path.is_dir() {
        return Err(format!("Mission repo_path '{repo_path_str}' is not a directory"));
    }

    // main_branch 缺失时探测
    let main_branch = match main_branch_opt {
        Some(b) if !b.is_empty() => b,
        _ => {
            let manager = WorktreeManager::new(repo_path.clone());
            tokio::task::spawn_blocking(move || manager.detect_main_branch())
                .await
                .map_err(|e| format!("detect_main_branch panicked: {e}"))?
                .map_err(|e| format!("detect_main_branch failed: {e}"))?
        }
    };

    let agent = ChatAgent::new(provider, model, repo_path, main_branch, app.clone());
    let summary = agent
        .handle_user_message(&mission_id, trimmed, force_direct)
        .await
        .map_err(|e| format!("{e:#}"))?;
    Ok(summary)
}

#[derive(Debug, Deserialize)]
pub struct ConfirmFollowupRequest {
    pub parent_mission_id: String,
    pub title: String,
    pub request_summary: String,
    /// 子 mission 复用父 mission 的 repo_path（在同一仓库继续工作）。
    /// 这里允许覆盖；缺省 → 取父 mission。
    #[serde(default)]
    pub repo_path_override: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ConfirmFollowupResponse {
    pub child_mission_id: String,
    pub repo_path: String,
}

/// FR-15.4: 用户确认后创建子 mission（draft 状态），关联 parent_mission_id。
/// 前端拿到 child_mission_id 后再调用 plan_mission 走完整 Planner 流程。
#[tauri::command]
pub fn confirm_followup_proposal(
    app: tauri::AppHandle,
    request: ConfirmFollowupRequest,
) -> Result<ConfirmFollowupResponse, String> {
    let db = app.state::<crate::db::Database>();
    let (parent_repo_path, parent_repo_origin) = db
        .with_conn(|c| {
            c.query_row(
                "SELECT repo_path, repo_origin FROM missions WHERE id = ?1",
                [&request.parent_mission_id],
                |r| Ok((
                    r.get::<_, Option<String>>(0)?,
                    r.get::<_, Option<String>>(1)?,
                )),
            )
            .map_err(anyhow::Error::from)
        })
        .map_err(|e| format!("Parent mission not found: {e}"))?;

    let repo_path = request
        .repo_path_override
        .or(parent_repo_path)
        .ok_or_else(|| "Parent mission has no repo_path".to_string())?;

    // 拼接子 mission 描述：用户原始请求 + 父 mission artifacts 摘要
    let artifacts = db
        .with_conn(|c| queries::list_artifacts_for_mission(c, &request.parent_mission_id))
        .unwrap_or_default();

    let mut artifact_md = String::new();
    if !artifacts.is_empty() {
        artifact_md.push_str("\n\n## Existing Artifacts (from parent mission)\n");
        for a in &artifacts {
            artifact_md.push_str(&format!(
                "- `{}` ({}): {}\n",
                a.local_name, a.artifact_type, a.summary
            ));
        }
    }
    let description = format!("{}{artifact_md}", request.request_summary.trim());

    let child_id = Uuid::new_v4().to_string();
    let repo_origin_for_child = parent_repo_origin.unwrap_or_else(|| "from_existing".to_string());

    db.with_conn(|c| {
        c.execute(
            "INSERT INTO missions (id, title, description, repo_origin, repo_path, parent_mission_id)
             VALUES (?, ?, ?, ?, ?, ?)",
            rusqlite::params![
                child_id,
                request.title,
                description,
                repo_origin_for_child,
                repo_path,
                request.parent_mission_id,
            ],
        )
        .map_err(anyhow::Error::from)
    })
    .map_err(|e| format!("Insert follow-up mission failed: {e}"))?;

    // 在 chat 历史里追加一条 system 消息表示已升级
    let _ = db.with_conn(|c| {
        let id = Uuid::new_v4().to_string();
        queries::insert_mission_chat(
            c,
            &id,
            &request.parent_mission_id,
            "system",
            &format!(
                "[escalated] User confirmed escalation. Created follow-up mission `{child_id}` — {}",
                request.title
            ),
            None,
            None,
            Some(&child_id),
        )
    });

    // FM-14: 把对应的 approval row（kind=escalation, chat_message_id=...）顺手 resolve 掉，
    // 避免它继续在 ApprovalQueue 里挂着。
    use tauri::Emitter;
    let matched: Vec<String> = db
        .with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT id FROM approval_requests
                 WHERE status='pending' AND kind='escalation' AND chat_message_id IS NOT NULL
                    AND mission_id = ?1
                 ORDER BY created_at DESC LIMIT 1",
            )?;
            let ids = stmt
                .query_map([&request.parent_mission_id], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<String>>>()?;
            Ok(ids)
        })
        .unwrap_or_default();
    for rid in matched {
        let _ = db.with_conn(|c| {
            queries::resolve_approval(c, &rid, "approved", "user", None)
                .map_err(anyhow::Error::from)
        });
        if let Some(coord) =
            app.try_state::<std::sync::Arc<crate::agent::approval::ApprovalCoordinator>>()
        {
            let coord_clone = coord.inner().clone();
            let rid_clone = rid.clone();
            tauri::async_runtime::spawn(async move {
                coord_clone.forget(&rid_clone).await;
            });
        }
        let _ = app.emit(
            "approval-resolved",
            serde_json::json!({
                "request_id": rid,
                "status": "approved",
                "decided_by": "user",
            }),
        );
    }

    Ok(ConfirmFollowupResponse {
        child_mission_id: child_id,
        repo_path,
    })
}

#[derive(Debug, Deserialize)]
pub struct RejectFollowupRequest {
    pub mission_id: String,
}

#[tauri::command]
pub fn reject_followup_proposal(
    app: tauri::AppHandle,
    request: RejectFollowupRequest,
) -> Result<(), String> {
    let db = app.state::<crate::db::Database>();
    db.with_conn(|c| {
        let id = Uuid::new_v4().to_string();
        queries::insert_mission_chat(
            c,
            &id,
            &request.mission_id,
            "system",
            "[rejected] User declined escalation. The next chat message will be executed \
             directly with `force_direct=true`.",
            None,
            None,
            None,
        )
    })
    .map_err(|e| e.to_string())?;

    // FM-14: 同步把对应 approval row 标 rejected。
    use tauri::Emitter;
    let matched: Vec<String> = db
        .with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT id FROM approval_requests
                 WHERE status='pending' AND kind='escalation' AND mission_id = ?1
                 ORDER BY created_at DESC LIMIT 1",
            )?;
            let ids = stmt
                .query_map([&request.mission_id], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<String>>>()?;
            Ok(ids)
        })
        .unwrap_or_default();
    for rid in matched {
        let _ = db.with_conn(|c| {
            queries::resolve_approval(c, &rid, "rejected", "user", None)
                .map_err(anyhow::Error::from)
        });
        if let Some(coord) =
            app.try_state::<std::sync::Arc<crate::agent::approval::ApprovalCoordinator>>()
        {
            let coord_clone = coord.inner().clone();
            let rid_clone = rid.clone();
            tauri::async_runtime::spawn(async move {
                coord_clone.forget(&rid_clone).await;
            });
        }
        let _ = app.emit(
            "approval-resolved",
            serde_json::json!({
                "request_id": rid,
                "status": "rejected",
                "decided_by": "user",
            }),
        );
    }
    Ok(())
}
