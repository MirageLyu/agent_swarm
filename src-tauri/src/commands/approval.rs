//! FM-14: Approval Queue IPC commands.
//!
//! 命令清单：
//! - `list_pending_approvals(mission_id?)`     -> 列表（前端 ApprovalQueue 渲染）
//! - `get_approval(request_id)`                -> 单条详情
//! - `resolve_approval(request_id, decision, note?)` -> 用户点 Approve / Reject
//! - `resolve_all_approvals(mission_id, decision)`   -> 批量同决定
//!
//! 前端拿到 outcome 后无需自己刷库——后端在 resolve 内部 emit `approval-resolved`，
//! 整个面板订阅这个事件即可。

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::{Emitter, Manager, State};

use crate::agent::approval::{ApprovalCoordinator, ApprovalDecision};
use crate::db::{queries, Database};

#[derive(Debug, Clone, Serialize)]
pub struct ApprovalView {
    pub id: String,
    pub mission_id: String,
    pub kind: String,
    pub agent_id: Option<String>,
    pub planner_session_id: Option<String>,
    pub chat_message_id: Option<String>,
    pub title: String,
    /// JSON 字符串，前端按 kind 解析（保持原样透传）。
    pub payload: String,
    pub reason: String,
    pub context_summary: String,
    pub status: String,
    pub decision_note: Option<String>,
    pub decided_by: Option<String>,
    pub resolved_at: Option<String>,
    pub expires_at: String,
    pub created_at: String,
}

impl From<queries::ApprovalRow> for ApprovalView {
    fn from(r: queries::ApprovalRow) -> Self {
        Self {
            id: r.id,
            mission_id: r.mission_id,
            kind: r.kind,
            agent_id: r.agent_id,
            planner_session_id: r.planner_session_id,
            chat_message_id: r.chat_message_id,
            title: r.title,
            payload: r.payload,
            reason: r.reason,
            context_summary: r.context_summary,
            status: r.status,
            decision_note: r.decision_note,
            decided_by: r.decided_by,
            resolved_at: r.resolved_at,
            expires_at: r.expires_at,
            created_at: r.created_at,
        }
    }
}

#[tauri::command]
pub fn list_pending_approvals(
    app: tauri::AppHandle,
    mission_id: Option<String>,
) -> Result<Vec<ApprovalView>, String> {
    let db = app.state::<Database>();
    let rows = db
        .with_conn(|c| queries::list_pending_approvals(c, mission_id.as_deref()))
        .map_err(|e| e.to_string())?;
    Ok(rows.into_iter().map(ApprovalView::from).collect())
}

#[tauri::command]
pub fn get_approval(
    app: tauri::AppHandle,
    request_id: String,
) -> Result<Option<ApprovalView>, String> {
    let db = app.state::<Database>();
    let row = db
        .with_conn(|c| queries::get_approval(c, &request_id))
        .map_err(|e| e.to_string())?;
    Ok(row.map(ApprovalView::from))
}

#[derive(Debug, Deserialize)]
pub struct ResolveApprovalRequest {
    pub request_id: String,
    /// 仅接受 "approved" | "rejected"；其余由后端自身 sweep 写。
    pub decision: String,
    pub note: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ResolveApprovalResponse {
    /// 是否成功唤醒等待方；false 表示该 request_id 已被解决（过期/取消/重复点击）。
    pub delivered: bool,
    /// 解决后的最终状态（数据库视角）。
    pub final_status: String,
}

#[tauri::command]
pub async fn resolve_approval(
    app: tauri::AppHandle,
    coord: State<'_, Arc<ApprovalCoordinator>>,
    request: ResolveApprovalRequest,
) -> Result<ResolveApprovalResponse, String> {
    let decision = parse_user_decision(&request.decision)?;

    let db = app.state::<Database>();

    // 1) DB-first 写入：保证刷新看到最终状态；失败说明已被 sweep / cancel 抢先。
    //    注意 set_agent_running 由协调器 submit_and_wait 兜底，这里不再处理。
    let won = db
        .with_conn(|c| {
            queries::resolve_approval(
                c,
                &request.request_id,
                decision.as_db_status(),
                "user",
                request.note.as_deref(),
            )
        })
        .map_err(|e| e.to_string())?;

    let final_status = if won {
        decision.as_db_status().to_string()
    } else {
        // 已是 expired / cancelled / approved / rejected 之一——读回最新值
        match db
            .with_conn(|c| queries::get_approval(c, &request.request_id))
            .map_err(|e| e.to_string())?
        {
            Some(row) => row.status,
            None => return Err(format!("approval `{}` not found", request.request_id)),
        }
    };

    // 2) 唤醒等待槽。即便 DB 没赢也尝试一次（防御性，coord.resolve 内会自动忽略不存在的 id）。
    let delivered = coord
        .resolve(&request.request_id, decision, request.note.clone())
        .await;

    // 3) 广播事件让所有打开的面板刷新。
    let _ = app.emit(
        "approval-resolved",
        serde_json::json!({
            "request_id": request.request_id,
            "status": final_status,
            "decided_by": "user",
            "note": request.note,
        }),
    );

    Ok(ResolveApprovalResponse {
        delivered,
        final_status,
    })
}

#[derive(Debug, Deserialize)]
pub struct ResolveAllApprovalsRequest {
    pub mission_id: String,
    /// "approved" | "rejected"
    pub decision: String,
    pub note: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ResolveAllApprovalsResponse {
    pub resolved_count: usize,
}

/// 方便 dev / debug 场景的批量决定（"全部同意" / "全部拒绝"）。
/// 生产 UI 默认隐藏，只有展开 advanced 才会显示。
#[tauri::command]
pub async fn resolve_all_approvals(
    app: tauri::AppHandle,
    coord: State<'_, Arc<ApprovalCoordinator>>,
    request: ResolveAllApprovalsRequest,
) -> Result<ResolveAllApprovalsResponse, String> {
    let decision = parse_user_decision(&request.decision)?;
    let db = app.state::<Database>();

    let pending = db
        .with_conn(|c| queries::list_pending_approvals(c, Some(&request.mission_id)))
        .map_err(|e| e.to_string())?;

    let mut count = 0usize;
    for row in pending {
        let won = db
            .with_conn(|c| {
                queries::resolve_approval(
                    c,
                    &row.id,
                    decision.as_db_status(),
                    "user",
                    request.note.as_deref(),
                )
            })
            .unwrap_or(false);
        if won {
            count += 1;
        }
        coord
            .resolve(&row.id, decision.clone(), request.note.clone())
            .await;
        let _ = app.emit(
            "approval-resolved",
            serde_json::json!({
                "request_id": row.id,
                "status": decision.as_db_status(),
                "decided_by": "user",
                "note": request.note,
            }),
        );
    }

    Ok(ResolveAllApprovalsResponse {
        resolved_count: count,
    })
}

fn parse_user_decision(raw: &str) -> Result<ApprovalDecision, String> {
    match raw {
        "approved" | "approve" => Ok(ApprovalDecision::Approved),
        "rejected" | "reject" => Ok(ApprovalDecision::Rejected),
        other => Err(format!(
            "Invalid decision `{other}`; only `approved` or `rejected` accepted from the client"
        )),
    }
}
