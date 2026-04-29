//! FM-15 FR-05.x: Planner `fetch_url` 用户确认 IPC（FM-14 桥接版）。
//!
//! 历史背景：
//! - FM-15 v2.2 引入了 `PlannerFetchCoordinator` + 旧 `PlannerFetchConfirmDialog`
//!   组件，IPC 命令 `confirm_planner_fetch` 把 `FetchDecision` (allow_once / allow_session /
//!   deny) 传回 oneshot。
//! - FM-14 上线统一审批队列后，fetch_url 改走 `ApprovalCoordinator`；旧前端弹窗仍然
//!   存在（短期不删，避免一次砍太多 UI），所以这条 IPC 命令被改造成桥接：
//!   把旧三态 `FetchDecision` 翻译成新模型的 (decision, note)，再通过统一协调器
//!   resolve。`request_id` 字段不变 —— 它就是 `approval_requests.id`。
//!
//! 当 `PlannerFetchConfirmDialog` 在 Slice 4 被 `ApprovalCard` 完全替代后，这个文件
//! 应该可以连同前端弹窗一起删除。

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::{Emitter, Manager, State};

use crate::agent::approval::{ApprovalCoordinator, ApprovalDecision};
use crate::agent::planner_fetch::FetchDecision;
use crate::db::{queries, Database};

#[derive(Debug, Deserialize)]
pub struct ConfirmPlannerFetchRequest {
    pub request_id: String,
    pub decision: FetchDecision,
}

#[derive(Debug, Serialize)]
pub struct ConfirmPlannerFetchResponse {
    /// 若 false：该 request_id 已经被 deny / timeout / 已被其它入口处理（例如 ApprovalQueue）。
    pub delivered: bool,
}

#[tauri::command]
pub async fn confirm_planner_fetch(
    app: tauri::AppHandle,
    coord: State<'_, Arc<ApprovalCoordinator>>,
    request: ConfirmPlannerFetchRequest,
) -> Result<ConfirmPlannerFetchResponse, String> {
    let (approval_decision, note) = match request.decision {
        FetchDecision::AllowOnce => (ApprovalDecision::Approved, Some("once".to_string())),
        FetchDecision::AllowSession => (ApprovalDecision::Approved, Some("session".to_string())),
        FetchDecision::Deny => (ApprovalDecision::Rejected, None),
    };

    let db = app.state::<Database>();
    // DB-first，与 commands/approval.rs::resolve_approval 保持一致行为。
    let _ = db.with_conn(|c| {
        queries::resolve_approval(
            c,
            &request.request_id,
            approval_decision.as_db_status(),
            "user",
            note.as_deref(),
        )
    });
    let delivered = coord
        .resolve(&request.request_id, approval_decision.clone(), note.clone())
        .await;
    let _ = app.emit(
        "approval-resolved",
        serde_json::json!({
            "request_id": request.request_id,
            "status": approval_decision.as_db_status(),
            "decided_by": "user",
            "note": note,
        }),
    );
    Ok(ConfirmPlannerFetchResponse { delivered })
}
