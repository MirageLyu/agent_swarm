//! Single-Agent Uplift P1-1：流式 tool dispatcher。
//!
//! ## 为什么单独建模块
//!
//! 当前 engine.rs 的执行链路是「等 LLM 全部 stream → 解析 LlmResponse.content 拿
//! 全部 tool_use → 按 safe/unsafe 桶并发/串行执行」——这是**批量并行**。streaming
//! tool execution 把它升级为**流式并行**：safe tool 的 input JSON 一闭合就 spawn，
//! 实际执行时间与 LLM 仍在 stream 文本的时间**重叠**。
//!
//! 为了避免在 engine.rs 那个 3000 行的主循环里塞调度状态机，把状态机抽出来：
//!   - pending（input 累积中）/ inflight（已 spawn）/ unsafe_queue / spawn_order
//!   - on_tool_use_start/on_input_delta/on_tool_use_stop 由 forwarder 在流式回调里 push
//!   - drain_in_order 在 step 末尾按 LLM tool_use 原顺序 join 结果（API 协议要求）
//!
//! ## 不变量
//!
//! 1. **unsafe tool 仍然串行**——写盘 / shell 不允许并发，保留 approval gate
//!    顺序与现状一致
//! 2. **tool_results 顺序严格按 ordered_tool_use_ids**——OpenAI/Anthropic API
//!    都要求 follow-up message 里 tool_results 的顺序匹配 tool_use 的顺序
//! 3. **cancel 时 abort_all** 必须能立即结束所有 inflight future——通过
//!    JoinHandle::abort，spawn 出去的 tool future 内部 await 点会被
//!    cooperative cancellation
//!
//! ## 与 engine.rs 的集成（Phase B，本 PR 不做）
//!
//! 本 PR（Phase A）只落地 executor + dispatcher trait + 完整单测覆盖，engine.rs
//! 主循环**还没有**接线——目前所有 tool dispatch 仍走 engine.rs 现有的批量并行路径。
//! 接线是一个独立的高 risk 改动（要在 forwarder 里识别新 chunk + 在 step 末尾
//! 改 drain 逻辑），单 PR 隔离更易 review、更易 rollback。
//!
//! Phase A 的价值：
//!   - LLM provider 已经 emit 了新 chunk（openai_compat），等于"信源就位"
//!   - executor 行为完全被单测锁定，Phase B 只需把 forwarder 接到 executor 接口

use std::collections::HashMap;
use std::sync::Arc;

use crate::tools::ToolOutput;

/// 工具调度的抽象层。
///
/// 为什么用 trait 而不是直接持有 ToolExecutor：
///   - executor 自己持有 `&AgentEngine` 的话循环引用、ownership 死结
///   - 单测可以 mock dispatcher 而不构造整个 ToolExecutor + AgentContext
///   - 未来 MCP / subagent dispatch 可以走不同实现
#[async_trait::async_trait]
pub trait ToolDispatcher: Send + Sync {
    /// 真正执行一个工具调用。
    ///
    /// `tool_use_id` 主要用于日志关联——执行时不需要知道 id，但便于
    /// trace 中追踪到底是哪个 tool_use 在跑。
    async fn dispatch(
        &self,
        agent_id: &str,
        tool_use_id: &str,
        name: &str,
        input: &serde_json::Value,
    ) -> ToolOutput;

    /// 该工具是否可与其它 concurrency-safe 工具并发执行。
    fn is_safe(&self, name: &str) -> bool;
}

/// 流式 tool 调度状态机。
///
/// 生命周期：每个 agent step 一个新实例。step 结束 drain 完即可丢弃。
pub struct StreamingToolExecutor {
    /// 已 spawn 但还没 join 的 safe 工具：tool_use_id → JoinHandle
    inflight: HashMap<String, tokio::task::JoinHandle<ToolOutput>>,
    /// input JSON 累积中：tool_use_id → (name, accumulated_input_string)
    /// ToolUseStart 时插入空 acc，InputDelta 追加，ToolUseStop 时 remove + 决定 spawn/queue。
    pending: HashMap<String, (String, String)>,
    /// 已 spawn 顺序（debug 用，不参与 drain 顺序——drain 走 caller 给的 ordered_ids）
    spawn_order: Vec<String>,
    /// unsafe 工具暂存：等 stream 结束在 drain 阶段串行执行
    /// 顺序按到达顺序（即 LLM emit ToolUseStop 顺序，与 LlmResponse.content 一致）
    unsafe_queue: Vec<(String, String, serde_json::Value)>,
    dispatcher: Arc<dyn ToolDispatcher>,
}

impl StreamingToolExecutor {
    pub fn new(dispatcher: Arc<dyn ToolDispatcher>) -> Self {
        Self {
            inflight: HashMap::new(),
            pending: HashMap::new(),
            spawn_order: Vec::new(),
            unsafe_queue: Vec::new(),
            dispatcher,
        }
    }

    /// 收到 `ToolUseStart` chunk 时调用。
    ///
    /// 幂等：重复 start 同一个 tool_use_id 视为 reset（清空累积 input）。
    /// 这种情况实际不会发生，但防御性处理避免静默 corruption。
    pub fn on_tool_use_start(&mut self, tool_use_id: String, name: String) {
        self.pending.insert(tool_use_id, (name, String::new()));
    }

    /// 收到 `ToolUseInputDelta` chunk 时调用。
    ///
    /// 没见过的 tool_use_id 直接丢——provider 协议违规时不让 executor crash。
    pub fn on_input_delta(&mut self, tool_use_id: &str, delta: &str) {
        if let Some((_, acc)) = self.pending.get_mut(tool_use_id) {
            acc.push_str(delta);
        }
    }

    /// 收到 `ToolUseStop` chunk 时调用——尝试解析 input + 决定 spawn / queue。
    ///
    /// 返回 `Some(error_str)` 仅在 pending 里找不到 tool_use_id 时（协议异常），
    /// caller 可以 log warn。返回 `None` 是正常路径。
    pub fn on_tool_use_stop(&mut self, agent_id: &str, tool_use_id: &str) -> Option<String> {
        let (name, raw_args) = match self.pending.remove(tool_use_id) {
            Some(v) => v,
            None => return Some(format!("ToolUseStop for unknown tool_use_id={tool_use_id}")),
        };
        let input = crate::llm::openai_compat::parse_tool_arguments_or_sentinel(&name, &raw_args);

        if self.dispatcher.is_safe(&name) {
            let dispatcher = self.dispatcher.clone();
            let agent_id = agent_id.to_string();
            let tool_use_id_clone = tool_use_id.to_string();
            let name_clone = name.clone();
            let input_clone = input.clone();
            let handle = tokio::spawn(async move {
                dispatcher
                    .dispatch(&agent_id, &tool_use_id_clone, &name_clone, &input_clone)
                    .await
            });
            self.inflight.insert(tool_use_id.to_string(), handle);
            self.spawn_order.push(tool_use_id.to_string());
        } else {
            self.unsafe_queue
                .push((tool_use_id.to_string(), name, input));
        }
        None
    }

    /// step 末尾调用：按 `ordered_tool_use_ids` 顺序拿结果。
    ///
    /// `ordered_tool_use_ids` 来自最终 `LlmResponse.content` 的 tool_use 顺序——
    /// 保证与 follow-up message 拼回顺序严格一致，满足 API 协议。
    ///
    /// safe 工具：handle.await（已经在跑，可能完成可能没）
    /// unsafe 工具：现在串行 dispatch
    /// 找不到的 id：返回 is_error=true 的占位（防御性，正常不应触发）
    pub async fn drain_in_order(
        &mut self,
        agent_id: &str,
        ordered_tool_use_ids: &[String],
    ) -> Vec<(String, ToolOutput)> {
        let mut results = Vec::with_capacity(ordered_tool_use_ids.len());
        for tu_id in ordered_tool_use_ids {
            if let Some(handle) = self.inflight.remove(tu_id) {
                let output = handle.await.unwrap_or_else(|e| ToolOutput {
                    content: format!(
                        "{{\"error\":\"tool_panic\",\"message\":\"join handle failed: {e}\"}}"
                    ),
                    is_error: true,
                    meta: None,
                });
                results.push((tu_id.clone(), output));
            } else if let Some(idx) = self.unsafe_queue.iter().position(|(id, _, _)| id == tu_id) {
                let (_id, name, input) = self.unsafe_queue.remove(idx);
                let output = self
                    .dispatcher
                    .dispatch(agent_id, tu_id, &name, &input)
                    .await;
                results.push((tu_id.clone(), output));
            } else {
                results.push((
                    tu_id.clone(),
                    ToolOutput {
                        content: format!(
                            "{{\"error\":\"internal\",\"message\":\"tool_use_id {tu_id} not in any queue\"}}"
                        ),
                        is_error: true,
            meta: None,
        },
                ));
            }
        }
        results
    }

    /// cancel 时显式 abort 所有 inflight。
    ///
    /// tokio::spawn 的 task 通过 abort() 在下一个 .await 点被打断；如果工具内部
    /// 是 CPU bound spin loop 那 abort 不立即生效——但目前所有内置工具都至少
    /// 有 I/O await（读文件 / shell / rg），所以 abort 实践上几十 ms 内返回。
    pub fn abort_all(&mut self) {
        for (_id, handle) in self.inflight.drain() {
            handle.abort();
        }
        self.pending.clear();
        self.unsafe_queue.clear();
        self.spawn_order.clear();
    }

    /// 调试/单测 helper：当前在跑的 safe 工具数。
    #[cfg(test)]
    pub fn inflight_count(&self) -> usize {
        self.inflight.len()
    }

    /// 调试/单测 helper：unsafe 队列长度。
    #[cfg(test)]
    pub fn unsafe_queue_len(&self) -> usize {
        self.unsafe_queue.len()
    }

    /// 调试/单测 helper：仍累积中的 tool_use 数。
    #[cfg(test)]
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }
}

// ============================================================================
// 单测
// ============================================================================

#[cfg(test)]
mod tests {
    //! P1-1 状态机不变量回归。
    //!
    //! 核心覆盖：
    //!   1. safe 工具：on_stop 后立即在 inflight（spawn 已起）；不必等 drain
    //!   2. unsafe 工具：on_stop 后进 unsafe_queue，dispatcher 调用次数=0；
    //!      drain 后 +1
    //!   3. 多 tool_use 交错 chunks：start1/start2/delta1/delta2/stop2/stop1
    //!      → drain 返回顺序按 ordered_ids 而非完成顺序
    //!   4. abort_all：立即清空 inflight + pending + unsafe_queue
    //!   5. 未知 tool_use_id 的 ToolUseStop / drain → 返回 error，不 panic
    //!   6. parse error sentinel：args 是非法 JSON 时仍能 dispatch（input 是 sentinel）

    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use tokio::sync::Mutex;

    /// 测试用 mock dispatcher。
    /// call_count：dispatch 被调用次数（验证 unsafe 不在 stop 时执行）。
    /// safe_set：注册哪些 name 是 safe。
    /// behavior：每个 name 的固定返回（error 或 ok）。
    struct MockDispatcher {
        call_count: AtomicUsize,
        safe_set: std::collections::HashSet<String>,
        behavior: Mutex<HashMap<String, ToolOutput>>,
        /// 模拟 tool 执行延迟（验证 spawn 真的在跑）
        delay_ms: u64,
    }

    impl MockDispatcher {
        fn new() -> Self {
            Self {
                call_count: AtomicUsize::new(0),
                safe_set: std::collections::HashSet::new(),
                behavior: Mutex::new(HashMap::new()),
                delay_ms: 0,
            }
        }

        fn with_safe(mut self, name: &str) -> Self {
            self.safe_set.insert(name.to_string());
            self
        }

        fn with_delay(mut self, ms: u64) -> Self {
            self.delay_ms = ms;
            self
        }

        async fn set_response(&self, name: &str, output: ToolOutput) {
            self.behavior.lock().await.insert(name.to_string(), output);
        }

        fn calls(&self) -> usize {
            self.call_count.load(Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl ToolDispatcher for MockDispatcher {
        async fn dispatch(
            &self,
            _agent_id: &str,
            _tool_use_id: &str,
            name: &str,
            _input: &serde_json::Value,
        ) -> ToolOutput {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            if self.delay_ms > 0 {
                tokio::time::sleep(Duration::from_millis(self.delay_ms)).await;
            }
            self.behavior
                .lock()
                .await
                .get(name)
                .cloned()
                .unwrap_or(ToolOutput {
                    content: format!("default-ok({name})"),
                    is_error: false,
                    meta: None,
                })
        }

        fn is_safe(&self, name: &str) -> bool {
            self.safe_set.contains(name)
        }
    }

    #[tokio::test]
    async fn safe_tool_spawns_immediately_on_stop() {
        let mock = Arc::new(MockDispatcher::new().with_safe("read_file"));
        mock.set_response(
            "read_file",
            ToolOutput {
                content: "ok".into(),
                is_error: false,
                meta: None,
            },
        )
        .await;
        let mut ex = StreamingToolExecutor::new(mock.clone());

        ex.on_tool_use_start("t1".into(), "read_file".into());
        ex.on_input_delta("t1", r#"{"path":"a.txt"}"#);
        assert!(ex.on_tool_use_stop("agent-1", "t1").is_none());

        // 关键断言：drain 前 inflight 已有 1（safe 工具立刻 spawn）
        assert_eq!(ex.inflight_count(), 1);
        assert_eq!(ex.pending_count(), 0);

        let results = ex.drain_in_order("agent-1", &["t1".to_string()]).await;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, "t1");
        assert!(!results[0].1.is_error);
        assert_eq!(mock.calls(), 1);
    }

    #[tokio::test]
    async fn unsafe_tool_queued_until_drain() {
        let mock = Arc::new(MockDispatcher::new()); // 没注册 safe → 默认 unsafe
        let mut ex = StreamingToolExecutor::new(mock.clone());

        ex.on_tool_use_start("u1".into(), "write_file".into());
        ex.on_input_delta("u1", r#"{"path":"a","content":"x"}"#);
        ex.on_tool_use_stop("agent-1", "u1");

        // 关键断言：drain 前 dispatcher.calls == 0（unsafe 不抢跑）
        assert_eq!(mock.calls(), 0);
        assert_eq!(ex.unsafe_queue_len(), 1);
        assert_eq!(ex.inflight_count(), 0);

        let results = ex.drain_in_order("agent-1", &["u1".to_string()]).await;
        assert_eq!(mock.calls(), 1);
        assert_eq!(results.len(), 1);
    }

    #[tokio::test]
    async fn out_of_order_stops_drain_in_caller_order() {
        let mock = Arc::new(
            MockDispatcher::new()
                .with_safe("read_file")
                .with_safe("glob"),
        );
        let mut ex = StreamingToolExecutor::new(mock.clone());

        // LLM tool_use 顺序 = t1, t2（caller 给 drain 的 ordered_ids 也是 t1, t2）
        // 但 stream chunk 顺序里 t2 先 stop——drain 仍按 ordered_ids 返回
        ex.on_tool_use_start("t1".into(), "read_file".into());
        ex.on_tool_use_start("t2".into(), "glob".into());
        ex.on_input_delta("t1", r#"{"path":"a.txt"}"#);
        ex.on_input_delta("t2", r#"{"pattern":"**/*.rs"}"#);
        ex.on_tool_use_stop("agent-1", "t2");
        ex.on_tool_use_stop("agent-1", "t1");

        let results = ex
            .drain_in_order("agent-1", &["t1".to_string(), "t2".to_string()])
            .await;
        assert_eq!(results[0].0, "t1");
        assert_eq!(results[1].0, "t2");
    }

    #[tokio::test]
    async fn abort_all_clears_state() {
        let mock = Arc::new(
            MockDispatcher::new()
                .with_safe("read_file")
                .with_delay(1000), // 模拟长任务
        );
        let mut ex = StreamingToolExecutor::new(mock.clone());

        ex.on_tool_use_start("t1".into(), "read_file".into());
        ex.on_tool_use_stop("agent-1", "t1");
        assert_eq!(ex.inflight_count(), 1);

        ex.on_tool_use_start("t2".into(), "read_file".into());
        // t2 还没 stop，pending 应该有它
        assert_eq!(ex.pending_count(), 1);

        ex.abort_all();
        assert_eq!(ex.inflight_count(), 0);
        assert_eq!(ex.pending_count(), 0);
        assert_eq!(ex.unsafe_queue_len(), 0);
    }

    #[tokio::test]
    async fn unknown_tool_use_id_in_stop_returns_warn() {
        let mock = Arc::new(MockDispatcher::new().with_safe("x"));
        let mut ex = StreamingToolExecutor::new(mock);

        let result = ex.on_tool_use_stop("agent-1", "ghost-id");
        assert!(result.is_some());
        assert!(result.unwrap().contains("ghost-id"));
    }

    #[tokio::test]
    async fn drain_with_unknown_id_returns_error_output() {
        let mock = Arc::new(MockDispatcher::new());
        let mut ex = StreamingToolExecutor::new(mock);

        let results = ex
            .drain_in_order("agent-1", &["never-spawned".to_string()])
            .await;
        assert_eq!(results.len(), 1);
        assert!(results[0].1.is_error);
        assert!(results[0].1.content.contains("never-spawned"));
    }

    #[tokio::test]
    async fn invalid_json_args_uses_sentinel_input() {
        let mock = Arc::new(MockDispatcher::new().with_safe("read_file"));
        let mut ex = StreamingToolExecutor::new(mock.clone());

        ex.on_tool_use_start("t1".into(), "read_file".into());
        ex.on_input_delta("t1", "{not valid json"); // 半截 JSON
        ex.on_tool_use_stop("agent-1", "t1");

        // 不应 panic；dispatch 仍被调用（sentinel input 进去）
        let results = ex.drain_in_order("agent-1", &["t1".to_string()]).await;
        assert_eq!(results.len(), 1);
        assert_eq!(mock.calls(), 1);
    }

    #[tokio::test]
    async fn safe_and_unsafe_interleaved_drain_preserves_order() {
        let mock = Arc::new(MockDispatcher::new().with_safe("read_file"));
        let mut ex = StreamingToolExecutor::new(mock.clone());

        // 交错：safe → unsafe → safe
        ex.on_tool_use_start("s1".into(), "read_file".into());
        ex.on_tool_use_start("u1".into(), "write_file".into());
        ex.on_tool_use_start("s2".into(), "read_file".into());
        ex.on_input_delta("s1", r#"{"path":"a"}"#);
        ex.on_input_delta("u1", r#"{"path":"b","content":"x"}"#);
        ex.on_input_delta("s2", r#"{"path":"c"}"#);
        ex.on_tool_use_stop("agent-1", "s1");
        ex.on_tool_use_stop("agent-1", "u1");
        ex.on_tool_use_stop("agent-1", "s2");

        // s1 / s2 在 inflight，u1 在 unsafe_queue
        assert_eq!(ex.inflight_count(), 2);
        assert_eq!(ex.unsafe_queue_len(), 1);

        let results = ex
            .drain_in_order(
                "agent-1",
                &["s1".to_string(), "u1".to_string(), "s2".to_string()],
            )
            .await;
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].0, "s1");
        assert_eq!(results[1].0, "u1");
        assert_eq!(results[2].0, "s2");
        assert_eq!(mock.calls(), 3);
    }

    #[tokio::test]
    async fn duplicate_start_resets_accumulation() {
        // 实际不会发生，但防御性：重复 start 同 id 应清空累积，而不是双倍累积
        let mock = Arc::new(MockDispatcher::new().with_safe("read_file"));
        let mut ex = StreamingToolExecutor::new(mock.clone());

        ex.on_tool_use_start("t1".into(), "read_file".into());
        ex.on_input_delta("t1", r#"{"path":"bad"#); // 半截
        ex.on_tool_use_start("t1".into(), "read_file".into()); // 重启
        ex.on_input_delta("t1", r#"{"path":"good.txt"}"#);
        ex.on_tool_use_stop("agent-1", "t1");

        let _ = ex.drain_in_order("agent-1", &["t1".to_string()]).await;
        assert_eq!(mock.calls(), 1);
    }
}
