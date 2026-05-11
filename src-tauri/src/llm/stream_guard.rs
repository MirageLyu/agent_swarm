//! 流式 LLM 调用的"长沉默"看门狗（Issue 5 通用化）。
//!
//! # 为什么不放进 LlmProvider trait 自身
//!
//! 看门狗的"什么算活动"是 transport-agnostic 的（任意 chunk 都算），但是否需要
//! watchdog、阈值多少、超时后该返回什么类型的错误，是**调用方**关心的策略。
//! 把它做成一个独立的小 helper，调用方在原本写
//!
//! ```ignore
//! let h = tokio::spawn(async move { provider.stream_chat(&req, tx).await });
//! ```
//!
//! 的位置改写成
//!
//! ```ignore
//! let h = tokio::spawn(stream_chat_with_idle_guard(
//!     provider, req, tx, DEFAULT_STREAM_IDLE_TIMEOUT,
//! ));
//! ```
//!
//! 即可获得统一的"长沉默 abort + 友好错误"行为。
//!
//! # 看门狗策略
//!
//! - **任意 chunk** 都算活动（TextDelta / ReasoningDelta / ToolUseDelta / MessageStop 等）。
//!   推理模型 thinking 阶段虽然没有 TextDelta，但 ReasoningDelta 也会 reset 计时器。
//! - 超过 `idle_timeout` 没有相邻 chunk → abort 内部 stream task + forwarder task，
//!   返回 `StreamGuardError::IdleTimeout`。
//! - 默认阈值 `DEFAULT_STREAM_IDLE_TIMEOUT = 180s`，覆盖 DeepSeek-R1 / V4-Pro 等
//!   推理模型 thinking 阶段单次静默 60~120s 才出 first token 的场景。
//!
//! # 不在这里管的事
//!
//! - **总 wall-clock 上限**：调用方自己用 `tokio::time::timeout` 包一层，因为不同
//!   工作流总时长预期差很多（Pre-flight 单轮 60s，Planner mission 600s，长 agent
//!   循环可能 1800s）。
//! - **重试**：调用方决定。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use thiserror::Error;
use tokio::sync::mpsc;

use super::{LlmProvider, LlmRequest, LlmResponse, StreamChunk};

/// 默认空闲超时：180s。
///
/// 推理模型（DeepSeek-R1 / V4-Pro / QwQ）thinking 阶段单次可静默 60~120s 才出
/// first token，180s 留 50% 余量。再长就大概率是 provider / 网络真挂了。
pub const DEFAULT_STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(180);

/// Watchdog 最大检查间隔。实际值 = `min(idle_timeout / 6, this)`，
/// 让短阈值场景（如测试用的 100ms idle）也能及时响应而不至于一直 sleep。
const IDLE_CHECK_INTERVAL_MAX: Duration = Duration::from_secs(10);

fn watchdog_check_interval(idle_timeout: Duration) -> Duration {
    // 1/6 = 让 watchdog 在 idle 阈值期间最多检查 6 次，足够精确；
    // 上限 10s 避免线上长 idle 阈值（如 180s）也每秒醒来浪费 CPU。
    let proposed = idle_timeout / 6;
    if proposed < IDLE_CHECK_INTERVAL_MAX {
        proposed.max(Duration::from_millis(10))
    } else {
        IDLE_CHECK_INTERVAL_MAX
    }
}

#[derive(Debug, Error)]
pub enum StreamGuardError {
    #[error("LLM stream idle for {idle_secs}s (threshold {threshold_secs}s); aborted")]
    IdleTimeout { idle_secs: u64, threshold_secs: u64 },
    #[error("Stream task join error: {0}")]
    Join(String),
    #[error("LLM error: {0}")]
    Llm(String),
}

impl StreamGuardError {
    /// 给前端友好提示用的中文短句。各 caller 把它塞进自己的 user_msg / IpcError。
    pub fn user_message_zh(&self) -> String {
        match self {
            Self::IdleTimeout { idle_secs, .. } => {
                format!("LLM 响应卡住 {idle_secs}s 无新内容，已自动中止。请检查网络或重试。")
            }
            Self::Join(_) => "LLM 调用任务异常退出".into(),
            Self::Llm(e) => format!("LLM 调用失败：{e}"),
        }
    }
}

/// 包裹 `provider.stream_chat()` 加一层空闲看门狗 + chunk 透传。
///
/// 调用方传入 `downstream_tx`（与现有 `let (tx, rx) = mpsc::channel(...)` 中的
/// `tx` 同语义）。本函数：
/// 1. 在内部建立私有 channel 给 provider 写；
/// 2. 把每个 chunk 转发到 `downstream_tx`，同时刷新 `last_activity` 计时器；
/// 3. 周期性检查 idle（间隔自适应：`min(idle_timeout / 6, 10s)`，
///    短阈值时也能及时响应而不至于一直 sleep）；
/// 4. 超阈值就 abort 内部 stream task，返回 `IdleTimeout`；
/// 5. 否则返回最终 `LlmResponse`。
pub async fn stream_chat_with_idle_guard(
    provider: Arc<dyn LlmProvider>,
    request: LlmRequest,
    downstream_tx: mpsc::Sender<StreamChunk>,
    idle_timeout: Duration,
) -> Result<LlmResponse, StreamGuardError> {
    let check_interval = watchdog_check_interval(idle_timeout);
    // Provider 写入的私有 channel；buffer 256 与原各 caller 的设置对齐。
    let (inner_tx, mut inner_rx) = mpsc::channel::<StreamChunk>(256);
    let provider_clone = provider.clone();
    let req_clone = request.clone();
    let stream_handle =
        tokio::spawn(async move { provider_clone.stream_chat(&req_clone, inner_tx).await });

    // Activity counter：自看门狗启动以来的毫秒数。
    // 起始置为 0，第一个 chunk 抵达时才更新——这样首 token 也受看门狗保护
    // （thinking 模型可能首 token 就要 60+s）。
    let started_at = Instant::now();
    let last_activity = Arc::new(AtomicU64::new(0));
    let last_activity_fwd = last_activity.clone();
    let last_activity_wd = last_activity.clone();

    let forwarder = tokio::spawn(async move {
        while let Some(chunk) = inner_rx.recv().await {
            last_activity_fwd
                .store(started_at.elapsed().as_millis() as u64, Ordering::Relaxed);
            // downstream 关闭（caller 提前结束）→ 没必要继续转发，break。
            // 内部 stream task 下次写 inner_tx 也会因为 inner_rx drop 而失败，
            // provider 端会自然结束。
            if downstream_tx.send(chunk).await.is_err() {
                break;
            }
        }
    });

    let idle_watchdog = async move {
        loop {
            tokio::time::sleep(check_interval).await;
            let last = last_activity_wd.load(Ordering::Relaxed);
            let now = started_at.elapsed().as_millis() as u64;
            let idle_ms = now.saturating_sub(last);
            if idle_ms > idle_timeout.as_millis() as u64 {
                return idle_ms;
            }
        }
    };

    // 抓 abort handle：watchdog 触发时显式 cancel，否则它们会在后台续命
    // （HTTP 还在写、tx 还在发），既浪费 socket 也可能干扰下一轮。
    let stream_abort = stream_handle.abort_handle();
    let forwarder_abort = forwarder.abort_handle();

    let outcome = tokio::select! {
        res = stream_handle => {
            // stream 已自然结束；让 forwarder 把 channel 残余 chunk 排干。
            let _ = forwarder.await;
            Ok(res)
        }
        idle_ms = idle_watchdog => {
            stream_abort.abort();
            forwarder_abort.abort();
            Err(idle_ms)
        }
    };

    match outcome {
        Ok(join_res) => {
            let llm_res = join_res.map_err(|e| StreamGuardError::Join(e.to_string()))?;
            llm_res.map_err(|e| StreamGuardError::Llm(e.to_string()))
        }
        Err(idle_ms) => {
            tracing::error!(
                idle_ms,
                threshold_secs = idle_timeout.as_secs(),
                provider = provider.name(),
                model = %request.model,
                "stream chat idle timeout, aborted"
            );
            Err(StreamGuardError::IdleTimeout {
                idle_secs: idle_ms / 1000,
                threshold_secs: idle_timeout.as_secs(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    //! 测试用真实 tokio 时钟（不开 `test-util` feature 以免影响生产构建）。
    //! 所以阈值压到毫秒级 —— check interval 自适应，传 100ms idle 时它自动
    //! 用 ~16ms 周期检查，整组测试 < 2s 跑完。
    //!
    //! 关键不变量：
    //! 1. 持续有 chunk 时不超时；
    //! 2. 启动后长沉默（首 chunk 都不出）超阈值 → IdleTimeout；
    //! 3. 中段长沉默（first chunk 出了，后续卡住）也 → IdleTimeout
    //!    （这是用户实际遇到的场景）；
    //! 4. downstream receiver drop 不会导致 helper 假死。
    use super::*;
    use crate::llm::{ContentBlock, TokenUsage};
    use anyhow::Result;
    use async_trait::async_trait;
    use std::sync::Mutex;

    struct StubProvider {
        plan: Mutex<Option<Vec<(u64, StreamChunk)>>>,
    }

    impl StubProvider {
        fn new(plan: Vec<(u64, StreamChunk)>) -> Arc<Self> {
            Arc::new(Self {
                plan: Mutex::new(Some(plan)),
            })
        }
    }

    #[async_trait]
    impl LlmProvider for StubProvider {
        fn name(&self) -> &str {
            "stub"
        }
        async fn chat(&self, _request: &LlmRequest) -> Result<LlmResponse> {
            unimplemented!("stub.chat unused in tests")
        }
        async fn stream_chat(
            &self,
            _request: &LlmRequest,
            tx: mpsc::Sender<StreamChunk>,
        ) -> Result<LlmResponse> {
            let plan = self.plan.lock().unwrap().take().unwrap();
            for (delay_ms, chunk) in plan {
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                if tx.send(chunk).await.is_err() {
                    break;
                }
            }
            Ok(LlmResponse {
                content: vec![ContentBlock::Text { text: "ok".into() }],
                stop_reason: "end_turn".into(),
                usage: TokenUsage::default(),
            })
        }
        fn estimate_cost(&self, _m: &str, _i: u64, _o: u64) -> f64 {
            0.0
        }
    }

    fn dummy_chunk(text: &str) -> StreamChunk {
        StreamChunk {
            kind: crate::llm::StreamChunkKind::TextDelta,
            content: text.into(),
        }
    }

    fn dummy_request() -> LlmRequest {
        LlmRequest {
            model: "stub-model".into(),
            system: None,
            messages: vec![],
            tools: vec![],
            max_tokens: 100,
        }
    }

    /// 持续有 chunk → 不超时，正常返回 LlmResponse 且 downstream 收到所有 chunk。
    /// 持续有 chunk 时不超时。
    #[tokio::test]
    async fn streams_continuously_under_threshold() {
        let plan = vec![
            (10, dummy_chunk("a")),
            (10, dummy_chunk("b")),
            (10, dummy_chunk("c")),
        ];
        let provider = StubProvider::new(plan);
        let (tx, mut rx) = mpsc::channel::<StreamChunk>(8);

        let handle = tokio::spawn(stream_chat_with_idle_guard(
            provider,
            dummy_request(),
            tx,
            Duration::from_secs(5),
        ));

        let mut received = Vec::new();
        while let Some(c) = rx.recv().await {
            received.push(c.content);
        }

        let response = handle.await.unwrap().expect("应正常结束");
        assert_eq!(received, vec!["a", "b", "c"]);
        assert_eq!(response.stop_reason, "end_turn");
    }

    /// downstream receiver drop 后 forwarder 应该退出，整个 stream 不应阻塞。
    /// 这是 caller 提前结束（取消、错误）的常见场景，必须能干净收尾。
    #[tokio::test]
    async fn forwarder_exits_when_downstream_dropped() {
        let plan = vec![
            (5, dummy_chunk("a")),
            (5, dummy_chunk("b")),
            (5, dummy_chunk("c")),
        ];
        let provider = StubProvider::new(plan);
        let (tx, rx) = mpsc::channel::<StreamChunk>(8);

        drop(rx);

        let handle = tokio::spawn(stream_chat_with_idle_guard(
            provider,
            dummy_request(),
            tx,
            Duration::from_secs(5),
        ));

        let response = handle.await.unwrap().expect("downstream 关闭不应导致 helper 失败");
        assert_eq!(response.stop_reason, "end_turn");
    }

    /// 启动后长沉默（首 chunk 都不出）→ IdleTimeout。
    /// 用 100ms idle，check_interval 自适应到 ~16ms，毫秒级测试快速跑完。
    #[tokio::test]
    async fn idle_timeout_fires_when_stream_silent() {
        let plan = vec![(2_000, dummy_chunk("late"))];
        let provider = StubProvider::new(plan);
        let (tx, mut rx) = mpsc::channel::<StreamChunk>(8);

        let handle = tokio::spawn(stream_chat_with_idle_guard(
            provider,
            dummy_request(),
            tx,
            Duration::from_millis(100),
        ));

        let _ = rx.recv().await;

        let result = handle.await.unwrap();
        assert!(
            matches!(result, Err(StreamGuardError::IdleTimeout { .. })),
            "首 chunk 前长沉默必须触发 IdleTimeout, got {result:?}"
        );
    }

    /// 中段长沉默（first chunk 出了，后续卡住）→ IdleTimeout。
    /// 这是用户报告 bug 的真实场景：thinking 阶段 first token 出来了，下一段卡住。
    #[tokio::test]
    async fn idle_timeout_fires_mid_stream() {
        let plan = vec![
            (10, dummy_chunk("first")),
            (2_000, dummy_chunk("never-arrives")),
        ];
        let provider = StubProvider::new(plan);
        let (tx, mut rx) = mpsc::channel::<StreamChunk>(8);

        let handle = tokio::spawn(stream_chat_with_idle_guard(
            provider,
            dummy_request(),
            tx,
            Duration::from_millis(150),
        ));

        let first = rx.recv().await.unwrap();
        assert_eq!(first.content, "first");
        while rx.recv().await.is_some() {}

        let result = handle.await.unwrap();
        assert!(
            matches!(result, Err(StreamGuardError::IdleTimeout { .. })),
            "first chunk 后的长沉默必须触发 IdleTimeout, got {result:?}"
        );
    }

    /// `watchdog_check_interval` 自适应：短 idle → 短间隔，长 idle → 封顶 10s。
    #[test]
    fn check_interval_scales_with_threshold() {
        assert_eq!(
            watchdog_check_interval(Duration::from_secs(180)),
            Duration::from_secs(10),
            "长阈值要封顶到 10s 避免每秒醒来"
        );
        let short = watchdog_check_interval(Duration::from_millis(100));
        assert!(
            (15..=18).contains(&short.as_millis()),
            "短阈值要细化到 1/6 阈值（约 16~17ms），got {short:?}"
        );
        assert_eq!(
            watchdog_check_interval(Duration::from_millis(1)),
            Duration::from_millis(10),
            "极短阈值不允许跌破 10ms 下限"
        );
    }
}
