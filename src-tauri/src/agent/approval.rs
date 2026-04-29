//! FM-14: Approval Queue 协调器。
//!
//! 设计要点：
//! - **统一入口**：所有人审批（tool 拦截 / Planner fetch_url / Chat propose / budget /
//!   chat_commit 软阈值）都走这里，落库到 `approval_requests`，前端在 `ApprovalQueue`
//!   组件统一展示。
//! - **DB 是 source of truth**：协调器自身只持有"等用户决定"的 oneshot 通道；表里的
//!   `status`、`expires_at`、`decision_note` 才是真实状态。前端刷新只查 DB。
//! - **三种解除路径**：
//!     1. 用户在前端点 Approve / Reject → `commands/approval.rs::resolve_approval` →
//!        `coord.resolve(req_id, decision)` → oneshot 唤醒等待 task。
//!     2. 后端清理协程定期扫 `expire_overdue_approvals` → 已不存在用户回路 →
//!        oneshot 也会因为 `forget` 被 drop，等待方收到 RecvError → 视为 Expired。
//!     3. Mission 被 stop / restart → `cancel_pending_approvals_for_mission` →
//!        类似 expire，等待方收到 RecvError → 视为 Cancelled。
//!
//! Watchdog 协调（重要、易踩坑）：
//! - `agent/engine.rs` 用 `tokio::time::timeout(agent_timeout_seconds, ...)` 兜底 wall-clock，
//!   但 *不* 在 approval 等待期间暂停。这是有意的简化决定（参见 FM-14 实施记录）：
//!     * approval 默认 timeout 600s = agent 默认 timeout 1800s 的 1/3，
//!       单次 approval 不会把 agent 顶到 wall-clock；
//!     * 连续 ≥ 3 次 approval 才可能打满，这种情况 LLM 大概率在循环死磕，
//!       wall-clock 触发是合理的；
//!     * 真要做 approval 暂停 wall-clock，需要把 engine 的 `timeout` 替换为
//!       手写 deadline + select! 循环，那是个独立 commit，不放进 FM-14。
//! - `tools/executor.rs::shell_exec` 的 idle / wall watchdog 是对*正在跑的子进程*
//!   计时；进入 approval 等待时 *没有* 子进程在跑，所以那层 watchdog 天然不受影响。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::{oneshot, Mutex};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::db::{queries, Database};

/// 审批默认超时——超过即 expired，等待方默认按 reject 处理。
///
/// 600s = 10min，足够用户切回应用看一眼；同时确保 < 默认 agent_timeout (1800s)
/// 以维护 watchdog 协调原则（见模块级 doc）。
pub const DEFAULT_APPROVAL_TIMEOUT_SECS: i64 = 600;

/// 用户对一次审批的决定。Expired / Cancelled 由系统侧产生。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    Approved,
    Rejected,
    Expired,
    Cancelled,
}

impl ApprovalDecision {
    pub fn as_db_status(&self) -> &'static str {
        match self {
            ApprovalDecision::Approved => "approved",
            ApprovalDecision::Rejected => "rejected",
            ApprovalDecision::Expired => "expired",
            ApprovalDecision::Cancelled => "cancelled",
        }
    }
}

/// 审批结果（含可选用户备注，rejected 时用于回灌 agent 上下文）。
#[derive(Debug, Clone)]
pub struct ApprovalOutcome {
    pub decision: ApprovalDecision,
    pub note: Option<String>,
}

/// 审批种类——与 `approval_requests.kind` CHECK 约束一致。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalKind {
    Tool,
    Fetch,
    Escalation,
    Budget,
    ChatCommit,
}

impl ApprovalKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            ApprovalKind::Tool => "tool",
            ApprovalKind::Fetch => "fetch",
            ApprovalKind::Escalation => "escalation",
            ApprovalKind::Budget => "budget",
            ApprovalKind::ChatCommit => "chat_commit",
        }
    }
}

/// 调用方提交一个审批请求时的全量参数。
#[derive(Debug, Clone)]
pub struct ApprovalRequestSpec {
    pub mission_id: String,
    pub kind: ApprovalKind,
    /// 与 kind 对应的来源标识；三者最多设一个非 None，未限制（DB 会兜底允许全 NULL）。
    pub agent_id: Option<String>,
    pub planner_session_id: Option<String>,
    pub chat_message_id: Option<String>,
    /// 一行短标题，例如 `"Write to package.json"` / `"Fetch oauth.net"`。
    pub title: String,
    /// JSON 字符串，结构由 kind 决定（前端按 kind 渲染）。
    pub payload: String,
    /// LLM 自述/系统判定的"为什么需要审批"。
    pub reason: String,
    /// 可选上下文摘要（最近一次 LLM 思考前 200 字、当前 task title 等）。
    pub context_summary: String,
    /// 自定义超时；None 用 `DEFAULT_APPROVAL_TIMEOUT_SECS`。
    pub timeout_seconds: Option<i64>,
}

impl ApprovalRequestSpec {
    pub fn new(
        mission_id: impl Into<String>,
        kind: ApprovalKind,
        title: impl Into<String>,
    ) -> Self {
        Self {
            mission_id: mission_id.into(),
            kind,
            agent_id: None,
            planner_session_id: None,
            chat_message_id: None,
            title: title.into(),
            payload: "{}".into(),
            reason: String::new(),
            context_summary: String::new(),
            timeout_seconds: None,
        }
    }
}

/// 全局协调器（`tauri::manage` 注册）。
///
/// 内部只维护"等待槽" oneshot map；所有持久化都由 `queries` 负责，超时清理由
/// 后台任务调用 `expire_overdue_approvals` 后再调本协调器 `forget`。
pub struct ApprovalCoordinator {
    pending: Mutex<HashMap<String, oneshot::Sender<ApprovalOutcome>>>,
}

impl ApprovalCoordinator {
    pub fn new() -> Arc<Self> {
        Arc::new(Self { pending: Mutex::new(HashMap::new()) })
    }

    /// 内部：注册等待槽。返回 receiver 给等待方。
    async fn register(&self, request_id: String) -> oneshot::Receiver<ApprovalOutcome> {
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(request_id, tx);
        rx
    }

    /// IPC / 命令侧：用户在前端点 Approve / Reject 时调用。
    /// 返回 false 表示 request_id 不在等待槽（可能已超时被清掉、或 mission 被取消）。
    pub async fn resolve(
        &self,
        request_id: &str,
        decision: ApprovalDecision,
        note: Option<String>,
    ) -> bool {
        let tx = self.pending.lock().await.remove(request_id);
        match tx {
            Some(sender) => sender.send(ApprovalOutcome { decision, note }).is_ok(),
            None => false,
        }
    }

    /// 后台清理 / mission 取消路径调用：把等待槽 drop 掉，等待方收到 RecvError。
    pub async fn forget(&self, request_id: &str) {
        self.pending.lock().await.remove(request_id);
    }

    /// 当前 pending 槽位数（用于诊断 / 测试）。
    pub async fn pending_count(&self) -> usize {
        self.pending.lock().await.len()
    }
}

/// 完整流程：写库 → 设置 agent waiting → 等待用户决议 → 写库 → 恢复 agent 状态 → 返回。
///
/// `cancel_token`：mission/agent 被外部 cancel 时立即返回 `Cancelled`，避免 stop 后还堵在
/// approval 上。
pub async fn submit_and_wait(
    coord: &ApprovalCoordinator,
    db: &Database,
    spec: &ApprovalRequestSpec,
    cancel_token: &CancellationToken,
) -> anyhow::Result<(String, ApprovalOutcome)> {
    let request_id = Uuid::new_v4().to_string();
    let timeout_secs = spec
        .timeout_seconds
        .unwrap_or(DEFAULT_APPROVAL_TIMEOUT_SECS)
        .max(1);

    // 1) 写 approval_requests 表
    {
        let new_req = queries::NewApproval {
            id: &request_id,
            mission_id: &spec.mission_id,
            kind: spec.kind.as_str(),
            agent_id: spec.agent_id.as_deref(),
            planner_session_id: spec.planner_session_id.as_deref(),
            chat_message_id: spec.chat_message_id.as_deref(),
            title: &spec.title,
            payload: &spec.payload,
            reason: &spec.reason,
            context_summary: &spec.context_summary,
            timeout_seconds: timeout_secs,
        };
        db.with_conn(|conn| queries::insert_approval(conn, &new_req))?;
    }

    // 2) 如果绑定了 agent，置 waiting_approval；恢复在 finally 段。
    if let Some(agent_id) = spec.agent_id.as_deref() {
        let agent_id = agent_id.to_string();
        let _ = db.with_conn(|conn| queries::set_agent_waiting_approval(conn, &agent_id));
    }

    // 3) 注册等待槽 + 等待解除（select on: oneshot / cancel / timeout）
    let rx = coord.register(request_id.clone()).await;

    let outcome = tokio::select! {
        biased;
        _ = cancel_token.cancelled() => ApprovalOutcome {
            decision: ApprovalDecision::Cancelled,
            note: None,
        },
        res = rx => match res {
            Ok(outcome) => outcome,
            // sender 被 drop（超时清理 / mission cancel）→ 视为 Expired，
            // 由调用上游具体决定接下来动作。
            Err(_) => ApprovalOutcome { decision: ApprovalDecision::Expired, note: None },
        },
        _ = tokio::time::sleep(Duration::from_secs(timeout_secs as u64)) => {
            ApprovalOutcome { decision: ApprovalDecision::Expired, note: None }
        }
    };

    // 4) 兜底：把状态写回库（resolve 路径上 commands 也会写一次，DB 行为是
    //    "WHERE status='pending'"，幂等不会双写）。
    let _ = db.with_conn(|conn| {
        queries::resolve_approval(
            conn,
            &request_id,
            outcome.decision.as_db_status(),
            match outcome.decision {
                ApprovalDecision::Approved | ApprovalDecision::Rejected => "user",
                ApprovalDecision::Expired => "auto_expire",
                ApprovalDecision::Cancelled => "auto_expire",
            },
            outcome.note.as_deref(),
        )
    });

    // 5) 主动放弃等待槽（防御：resolve 走过的不会留，但 timeout/cancel 可能没清）。
    coord.forget(&request_id).await;

    // 6) 恢复 agent.status —— 不论 outcome 如何，agent 都不再"waiting_approval"；
    //    实际 running / failed / cancelled 状态由调用方根据 outcome 自行更新。
    if let Some(agent_id) = spec.agent_id.as_deref() {
        let agent_id = agent_id.to_string();
        let _ = db.with_conn(|conn| queries::set_agent_running(conn, &agent_id));
    }

    Ok((request_id, outcome))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn coordinator_resolve_sends_outcome() {
        let coord = ApprovalCoordinator::new();
        let coord2 = coord.clone();
        let rx = coord.register("req-1".into()).await;
        assert_eq!(coord.pending_count().await, 1);

        let h = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            coord2
                .resolve("req-1", ApprovalDecision::Approved, Some("ok".into()))
                .await
        });

        let outcome = rx.await.unwrap();
        assert_eq!(outcome.decision, ApprovalDecision::Approved);
        assert_eq!(outcome.note.as_deref(), Some("ok"));
        assert!(h.await.unwrap());

        // 同一 id 第二次 resolve 应返回 false
        assert!(
            !coord
                .resolve("req-1", ApprovalDecision::Rejected, None)
                .await
        );
    }

    #[tokio::test]
    async fn coordinator_forget_drops_sender() {
        let coord = ApprovalCoordinator::new();
        let rx = coord.register("req-2".into()).await;
        coord.forget("req-2").await;
        assert_eq!(coord.pending_count().await, 0);
        assert!(rx.await.is_err());
    }

    #[tokio::test]
    async fn coordinator_unknown_resolve_returns_false() {
        let coord = ApprovalCoordinator::new();
        assert!(
            !coord
                .resolve("nope", ApprovalDecision::Approved, None)
                .await
        );
    }
}
