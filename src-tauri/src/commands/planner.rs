//! FM-15 FR-05.x: Planner `fetch_url` 用户确认 IPC。
//!
//! 前端在收到 `planner-fetch-confirmation` 事件后展示弹窗，用户点击
//! 「allow once / allow this session / deny」之后调用 `confirm_planner_fetch`，
//! 把决定通过 PlannerFetchCoordinator 送回正在等待的 tool call。

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::agent::planner_fetch::{FetchDecision, PlannerFetchCoordinator};

#[derive(Debug, Deserialize)]
pub struct ConfirmPlannerFetchRequest {
    pub request_id: String,
    pub decision: FetchDecision,
}

#[derive(Debug, Serialize)]
pub struct ConfirmPlannerFetchResponse {
    /// 若 false，表示该 request_id 已经被 deny / timeout / 提前完成。
    pub delivered: bool,
}

#[tauri::command]
pub async fn confirm_planner_fetch(
    coord: State<'_, Arc<PlannerFetchCoordinator>>,
    request: ConfirmPlannerFetchRequest,
) -> Result<ConfirmPlannerFetchResponse, String> {
    let delivered = coord.resolve(&request.request_id, request.decision).await;
    Ok(ConfirmPlannerFetchResponse { delivered })
}
