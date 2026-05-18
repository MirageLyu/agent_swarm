//! Single-Agent Uplift B1: AskUserQuestion 工具的 in-process 协调器。
//!
//! 设计：
//! - LLM 调 `ask_user_question` 工具 → engine.rs::dispatch_tool 截获 → 在
//!   [`PendingQuestions`] 全局表里注册一对 (session_id → oneshot::Sender<Answer>)；
//!   emit 一个 `system_hint` 事件给前端（meta.kind=`"ask_user_question"`），
//!   随后 await Receiver。
//! - 用户在前端选完 → IPC `submit_user_question_answer(session_id, answers)` →
//!   后端从全局表取 Sender，发出。Receiver 唤醒，把 answers 序列化成 JSON
//!   回给 LLM。
//! - **超时**：默认 30 分钟没等到答案就走"用户没回复"路径，让 LLM 自行决定
//!   （继续按默认值，或调用 task_complete 报告无法继续）。30 分钟选取依据：
//!   既能容纳"打个电话再回来选"的现实场景，又不会让 hung agent 永远挂着。
//! - **取消**：agent_cancel 触发 → engine 调 [`drop_pending`]，drop Sender 让
//!   Receiver 立刻 RecvError，工具返回结构化 cancellation 错误。
//!
//! 不直接耦合 ToolExecutor / AgentEngine 是有意的：sender 注册表是纯进程内
//! lookup，没有生命周期纠葛，也不需要传 Arc 到处走。

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

/// 用户对一个 session（可包含多 questions）的答复。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserAnswerSet {
    /// 与请求里 questions[].id 一一对应。值为用户选中的 option id 数组
    /// （即便 allow_multiple=false 也包成 1 元素数组，简化调用方）。
    pub answers: HashMap<String, Vec<String>>,
}

#[derive(Default)]
struct Inner {
    senders: HashMap<String, oneshot::Sender<UserAnswerSet>>,
}

static REGISTRY: OnceLock<Mutex<Inner>> = OnceLock::new();

fn registry() -> &'static Mutex<Inner> {
    REGISTRY.get_or_init(|| Mutex::new(Inner::default()))
}

/// 注册一个 session（一次 ask_user_question 调用的 question 集合）→
/// 返回对应的 Receiver。caller 应当 await 它。
pub fn register(session_id: &str) -> oneshot::Receiver<UserAnswerSet> {
    let (tx, rx) = oneshot::channel();
    let mut inner = registry().lock().unwrap();
    inner.senders.insert(session_id.to_string(), tx);
    rx
}

/// 用户提交答案：尝试投递。如果 session 已经过期 / 被取消 → Err（前端自己处理 toast）。
pub fn deliver(session_id: &str, answers: UserAnswerSet) -> Result<(), &'static str> {
    let sender = {
        let mut inner = registry().lock().unwrap();
        inner.senders.remove(session_id)
    };
    match sender {
        Some(tx) => tx.send(answers).map_err(|_| "receiver_dropped"),
        None => Err("unknown_or_expired_session"),
    }
}

/// 主动撤销 session（agent 取消时）。drop sender 让 Receiver 立刻 RecvError。
#[allow(dead_code)]
pub fn drop_pending(session_id: &str) {
    let mut inner = registry().lock().unwrap();
    inner.senders.remove(session_id);
}
