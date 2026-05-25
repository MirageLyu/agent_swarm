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

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use thiserror::Error;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::stream_diagnostics::{probe_endpoint, StatsSnapshot, StreamRegistry, StreamStats};
use super::{LlmProvider, LlmRequest, LlmResponse, StreamChunk};

/// 检测到 stream "看起来卡住"（首/中段静默达此值）时，**主动**做一次 in-process
/// probe + dump 全局活跃 stream 快照，落进 tracing 日志——这是下次复现时区分
/// "网络层抖动 vs reseller 卡 vs 多 stream 抢带宽"的关键证据点。
///
/// 比 abort 阈值（180s）更激进：用户感知开始疑神疑鬼是 30s 量级，但探测真正
/// 有诊断价值的窗口是"已经卡了 60s+ 但还没到 abort"——这时打 probe 才能区分
/// 真假故障。低于 60s 容易把推理模型的正常 thinking 误判成 stall。
const STALL_DIAGNOSTIC_TRIGGER: Duration = Duration::from_secs(60);

/// 实际触发阈值 = min(STALL_DIAGNOSTIC_TRIGGER, idle_timeout / 3)。
/// 让短阈值测试场景（idle_timeout 100~200ms）也能验证诊断路径，同时生产 idle_timeout=180s 时
/// 仍走 60s 这个语义阈值。
fn stall_diagnostic_trigger(idle_timeout: Duration) -> Duration {
    let one_third = idle_timeout / 3;
    if one_third < STALL_DIAGNOSTIC_TRIGGER {
        one_third
    } else {
        STALL_DIAGNOSTIC_TRIGGER
    }
}

/// 默认空闲超时：180s。
///
/// 推理模型（DeepSeek-R1 / V4-Pro / QwQ）thinking 阶段单次可静默 60~120s 才出
/// first token，180s 留 50% 余量。再长就大概率是 provider / 网络真挂了。
pub const DEFAULT_STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(180);

/// Single-Agent Uplift Phase 2.3: 用户**视觉**层面的"看起来卡住"门槛。
///
/// engine.rs 在 stream forwarder 旁边跑一个 heartbeat 任务，距上一个 chunk
/// 超过这个秒数就向前端 emit 一条 `tool_progress` 事件（"Waiting for LLM... idle 30s"），
/// 让用户感知到"agent 还活着，只是 LLM 在憋大招"。
///
/// 注意：plan.md 原定义为"30s 心跳 + 60s abort 两阶段"，**故意没把 abort 降到 60s**——
/// thinking 模型（DeepSeek-R1 / V4-Pro / QwQ）的首 token / step 间静默常超过 60s，
/// 砍下去会回归到老 issue：拉一个推理模型分分钟被 idle_timeout 杀。
/// abort 阈值仍走 `DEFAULT_STREAM_IDLE_TIMEOUT (180s)`，只把"心理学层面的卡住"
/// 提前到 30s 报给前端，用户感知 + 兼容性兼得。
pub const DEFAULT_STREAM_IDLE_HEARTBEAT_SECS: u64 = 30;

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
    /// 用户/上层主动取消（cancel_token 触发）。区别于 idle / network / llm 错误，
    /// 这是"正常的中断"，不需要重试或友好报错。
    #[error("LLM stream cancelled by user")]
    Cancelled,
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
            Self::Cancelled => "已取消".into(),
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
    // 不传 cancel_token 时，构造一个永不触发的占位 token——保持向后兼容。
    stream_chat_with_idle_guard_cancellable(
        provider,
        request,
        downstream_tx,
        idle_timeout,
        CancellationToken::new(),
    )
    .await
}

/// `stream_chat_with_idle_guard` 的可取消版本：除 idle 看门狗外还监听 `cancel_token`。
///
/// 用户点"停止"时 cancel_token 触发 → 立即 abort 内部 stream task 和 forwarder，
/// 返回 `StreamGuardError::Cancelled`。这条路径的关键在于**不等当前 chunk 完成**，
/// 也不等 idle_timeout（180s）兜底——直接 abort，让用户感知秒级响应。
pub async fn stream_chat_with_idle_guard_cancellable(
    provider: Arc<dyn LlmProvider>,
    request: LlmRequest,
    downstream_tx: mpsc::Sender<StreamChunk>,
    idle_timeout: Duration,
    cancel_token: CancellationToken,
) -> Result<LlmResponse, StreamGuardError> {
    stream_chat_with_idle_guard_full(
        provider,
        request,
        downstream_tx,
        idle_timeout,
        cancel_token,
        StreamRetryPolicy::default(),
    )
    .await
}

/// 网络错误重试策略。
///
/// **仅在"还没收到任何 chunk"时重试**——一旦 forwarder 已经吐过 chunk，
/// 重试会把同样内容再吐一遍给前端，UX 灾难。这种"中途断网"走 stream_chat
/// 内部 partial-fallback：保留已收到内容当成功结束，由上层 retry budget /
/// LLM 自行决定续写。
#[derive(Debug, Clone, Copy)]
pub struct StreamRetryPolicy {
    /// 最大重试次数。0 = 不重试。
    pub max_retries: u32,
    /// 首次退避；后续指数翻倍。
    pub initial_backoff: Duration,
    /// 退避上限，避免长 backoff 把用户晾着。
    pub max_backoff: Duration,
}

impl Default for StreamRetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 0,
            initial_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(16),
        }
    }
}

/// `stream_chat_with_idle_guard_cancellable` + 网络错误重试。
///
/// 行为契约：
/// - `Cancelled` / `IdleTimeout`：**永不重试**（用户/超时是终态）
/// - `Llm` 错误且 `chunks_received == 0`：按 `policy` 指数退避重试，
///   重试期间监听 `cancel_token`——用户取消立即返回 `Cancelled`
/// - `Llm` 错误且 `chunks_received > 0`：交给 stream_chat 内部 partial-fallback；
///   到这里说明 fallback 没救回来，直接返回原错误
/// - `Join`：极少见，按内部错误透传，不重试
pub async fn stream_chat_with_idle_guard_full(
    provider: Arc<dyn LlmProvider>,
    request: LlmRequest,
    downstream_tx: mpsc::Sender<StreamChunk>,
    idle_timeout: Duration,
    cancel_token: CancellationToken,
    policy: StreamRetryPolicy,
) -> Result<LlmResponse, StreamGuardError> {
    let mut attempt: u32 = 0;
    let mut backoff = policy.initial_backoff;
    loop {
        let chunks_received = Arc::new(AtomicU64::new(0));
        let res = stream_chat_with_idle_guard_inner(
            provider.clone(),
            request.clone(),
            downstream_tx.clone(),
            idle_timeout,
            cancel_token.clone(),
            chunks_received.clone(),
        )
        .await;

        match res {
            Ok(r) => return Ok(r),
            Err(StreamGuardError::Cancelled) => return Err(StreamGuardError::Cancelled),
            Err(StreamGuardError::IdleTimeout {
                idle_secs,
                threshold_secs,
            }) => {
                return Err(StreamGuardError::IdleTimeout {
                    idle_secs,
                    threshold_secs,
                });
            }
            Err(StreamGuardError::Join(e)) => return Err(StreamGuardError::Join(e)),
            Err(StreamGuardError::Llm(msg)) => {
                let received = chunks_received.load(Ordering::Relaxed);
                let can_retry =
                    received == 0 && attempt < policy.max_retries && !cancel_token.is_cancelled();
                if !can_retry {
                    if received > 0 {
                        tracing::warn!(
                            "stream chat error after {received} chunks; not retrying (would duplicate output): {msg}"
                        );
                    }
                    return Err(StreamGuardError::Llm(msg));
                }
                attempt += 1;
                tracing::warn!(
                    "stream chat connection error (attempt {attempt}/{}); retrying after {}ms: {msg}",
                    policy.max_retries,
                    backoff.as_millis()
                );
                // 退避期间也监听 cancel——用户在网络抖动期间点取消别让他等。
                tokio::select! {
                    _ = tokio::time::sleep(backoff) => {}
                    _ = cancel_token.cancelled() => return Err(StreamGuardError::Cancelled),
                }
                backoff = (backoff * 2).min(policy.max_backoff);
                continue;
            }
        }
    }
}

async fn stream_chat_with_idle_guard_inner(
    provider: Arc<dyn LlmProvider>,
    request: LlmRequest,
    downstream_tx: mpsc::Sender<StreamChunk>,
    idle_timeout: Duration,
    cancel_token: CancellationToken,
    chunks_received: Arc<AtomicU64>,
) -> Result<LlmResponse, StreamGuardError> {
    let check_interval = watchdog_check_interval(idle_timeout);
    // Provider 写入的私有 channel；buffer 256 与原各 caller 的设置对齐。
    let (inner_tx, mut inner_rx) = mpsc::channel::<StreamChunk>(256);
    let provider_clone = provider.clone();
    let req_clone = request.clone();

    // 注册到全局 StreamRegistry 让 stall 诊断能 dump 所有并发 stream 的状态。
    // RAII guard 确保任何路径退出（return / panic / abort）都会自动注销，
    // 不会在 registry 里留 ghost 条目。
    let endpoint = provider.endpoint_hint().unwrap_or_default();
    let stats = StreamStats::new(
        provider.name().to_string(),
        request.model.clone(),
        endpoint.clone(),
    );
    let _registry_guard = StreamRegistry::global().register(stats.clone());
    let stats_for_fwd = stats.clone();

    let stream_handle =
        tokio::spawn(async move { provider_clone.stream_chat(&req_clone, inner_tx).await });

    // Activity counter：自看门狗启动以来的毫秒数。
    // 起始置为 0，第一个 chunk 抵达时才更新——这样首 token 也受看门狗保护
    // （thinking 模型可能首 token 就要 60+s）。
    let started_at = Instant::now();
    let last_activity = Arc::new(AtomicU64::new(0));
    let last_activity_fwd = last_activity.clone();
    let last_activity_wd = last_activity.clone();

    let chunks_received_fwd = chunks_received.clone();
    let trace_provider = provider.name().to_string();
    let trace_model = request.model.clone();
    let forwarder = tokio::spawn(async move {
        let mut first_chunk_logged = false;
        let mut last_chunk_at_ms: u64 = 0;
        while let Some(chunk) = inner_rx.recv().await {
            let elapsed = started_at.elapsed().as_millis() as u64;
            last_activity_fwd.store(elapsed, Ordering::Relaxed);
            // 同步更新 registry 用的 stats，让 stall 时 dump 能看到 chunk 级 idle
            // （而不是 byte 级——byte 级在 provider 内部更新，更精确）。
            stats_for_fwd.record_chunk(
                if matches!(chunk.kind, super::types::StreamChunkKind::TextDelta) {
                    chunk.content.len() as u64
                } else {
                    0
                },
                if matches!(chunk.kind, super::types::StreamChunkKind::ReasoningDelta) {
                    chunk.content.len() as u64
                } else {
                    0
                },
            );

            if !first_chunk_logged {
                first_chunk_logged = true;
                tracing::debug!(
                    provider = %trace_provider,
                    model = %trace_model,
                    first_chunk_ms = elapsed,
                    "stream_guard saw first chunk from provider"
                );
            } else {
                // chunk 间隔异常 warn：>5s 间隔意味着 stream 进入"半卡"状态，
                // 比直接 idle_timeout (180s) 提早 36 倍记录到现场——下次 stall
                // 复盘时能看到"从哪个 chunk 开始变慢"。
                let gap = elapsed.saturating_sub(last_chunk_at_ms);
                if gap >= 5000 {
                    tracing::warn!(
                        provider = %trace_provider,
                        model = %trace_model,
                        chunk_gap_ms = gap,
                        chunks_so_far = chunks_received_fwd.load(Ordering::Relaxed) + 1,
                        "stream_guard detected long inter-chunk gap (>=5s); stream entering stall-like state"
                    );
                }
            }
            last_chunk_at_ms = elapsed;
            // 标记"已经吐过 chunk 给 downstream"——重试层用这个判断不能重试。
            chunks_received_fwd.fetch_add(1, Ordering::Relaxed);
            // downstream 关闭（caller 提前结束）→ 没必要继续转发，break。
            // 内部 stream task 下次写 inner_tx 也会因为 inner_rx drop 而失败，
            // provider 端会自然结束。
            if downstream_tx.send(chunk).await.is_err() {
                break;
            }
        }
    });

    // **stall 诊断**：当 stream 静默达 STALL_DIAGNOSTIC_TRIGGER 但还没到 abort 阈值时，
    // 第一次也是唯一一次记录"现场快照 + 同 host 探测"。flag 防止反复触发刷屏。
    let diagnostic_fired = Arc::new(AtomicBool::new(false));
    let diagnostic_fired_wd = diagnostic_fired.clone();
    let probe_endpoint_wd = endpoint.clone();
    let probe_provider_wd = provider.name().to_string();
    let probe_model_wd = request.model.clone();

    let idle_watchdog = async move {
        loop {
            tokio::time::sleep(check_interval).await;
            let last = last_activity_wd.load(Ordering::Relaxed);
            let now = started_at.elapsed().as_millis() as u64;
            let idle_ms = now.saturating_sub(last);

            // 关键诊断点：进入 stall 前的"中等沉默期"（默认 60s+ 但还没到 180s abort）
            // 时跑一次 probe + dump，captures the moment 主 stream 开始挂起但还活着，
            // 区别"快速失败网络断"和"reseller 慢吞吞"。
            let trigger_ms = stall_diagnostic_trigger(idle_timeout).as_millis() as u64;
            if idle_ms >= trigger_ms && !diagnostic_fired_wd.load(Ordering::Relaxed) {
                diagnostic_fired_wd.store(true, Ordering::Relaxed);
                let endpoint = probe_endpoint_wd.clone();
                let provider_name = probe_provider_wd.clone();
                let model = probe_model_wd.clone();
                // 探测 + dump 在独立 task 跑，绝对不阻塞 watchdog 自己的循环。
                // 即使探测自己卡 8s，watchdog 仍按原节奏检查 idle_timeout。
                tokio::spawn(run_stall_diagnostics(
                    endpoint,
                    provider_name,
                    model,
                    idle_ms,
                ));
            }

            if idle_ms > idle_timeout.as_millis() as u64 {
                return idle_ms;
            }
        }
    };

    // 抓 abort handle：watchdog / cancel 触发时显式 cancel 内部 task，否则它们
    // 会在后台续命（HTTP 还在写、tx 还在发），既浪费 socket 也可能干扰下一轮。
    let stream_abort = stream_handle.abort_handle();
    let forwarder_abort = forwarder.abort_handle();

    enum Outcome {
        Done(Result<Result<LlmResponse, anyhow::Error>, tokio::task::JoinError>),
        Idle(u64),
        Cancelled,
    }

    let outcome = tokio::select! {
        res = stream_handle => {
            // stream 已自然结束；让 forwarder 把 channel 残余 chunk 排干。
            let _ = forwarder.await;
            Outcome::Done(res)
        }
        idle_ms = idle_watchdog => {
            stream_abort.abort();
            forwarder_abort.abort();
            Outcome::Idle(idle_ms)
        }
        _ = cancel_token.cancelled() => {
            stream_abort.abort();
            forwarder_abort.abort();
            Outcome::Cancelled
        }
    };

    match outcome {
        Outcome::Done(join_res) => {
            let llm_res = join_res.map_err(|e| StreamGuardError::Join(e.to_string()))?;
            llm_res.map_err(|e| StreamGuardError::Llm(e.to_string()))
        }
        Outcome::Idle(idle_ms) => {
            // abort 时再 dump 一次现场——和 60s 触发的 stall 诊断不同，这次是
            // "已经判死刑"的快照：所有当前活跃 stream 的最终状态，配合上面 60s 的
            // probe 结果可以拼出"故障开始 vs 故障终态"两个时间点。
            dump_active_streams_snapshot(
                "stream chat idle abort",
                idle_ms,
                provider.name(),
                &request.model,
            );
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
        Outcome::Cancelled => {
            tracing::info!(
                provider = provider.name(),
                model = %request.model,
                "stream chat cancelled by user, aborted"
            );
            Err(StreamGuardError::Cancelled)
        }
    }
}

/// **Stall 诊断**：第一次检测到 stream 静默 ≥ 60s 时调用。
///
/// 工作流：
/// 1. 立刻在日志里 dump 全局 `StreamRegistry` 快照——能直接看出当时有几条流在跑、
///    各自吞吐如何（区分"假说 B 多 stream 抢带宽"vs 单流卡住）
/// 2. 启动一次独立 socket 的 HTTP probe 到同 host，看网络层是否健康
///    （区分"假说 A/C 网络层问题"vs"假说 D 进程内/连接特异性"）
///
/// 这条函数不返回结果——一切以 `tracing::warn!`/`tracing::error!` 形式落到日志，
/// 由用户/开发者事后从 log 文件捞出来对照。这样不会拖慢主 stream 的任何路径。
async fn run_stall_diagnostics(
    endpoint: String,
    provider_name: String,
    model: String,
    idle_ms_at_trigger: u64,
) {
    dump_active_streams_snapshot(
        "stream chat entering stall (first 60s diagnostic)",
        idle_ms_at_trigger,
        &provider_name,
        &model,
    );

    if endpoint.is_empty() {
        tracing::warn!(
            provider = %provider_name,
            model = %model,
            "stall_diagnostic: provider has no endpoint_hint; skipping in-process probe"
        );
        return;
    }

    tracing::info!(
        provider = %provider_name,
        model = %model,
        endpoint = %endpoint,
        idle_ms_at_trigger,
        "stall_diagnostic: starting in-process probe to same host"
    );
    let outcome = probe_endpoint(&endpoint).await;
    if outcome.is_network_healthy() {
        // 探测连接 8s 内拿到合法响应（status < 500）→ **网络层是通的**，
        // 故障不在 A/C，更可能是该特定 stream 连接 卡死或上游对单 connection 限速。
        tracing::warn!(
            provider = %provider_name,
            model = %model,
            probe_url = %outcome.url,
            probe_elapsed_ms = outcome.elapsed_ms,
            probe_status = outcome.status.unwrap_or(0),
            verdict = "NETWORK_HEALTHY_BUT_MAIN_STREAM_STALLED",
            "stall_diagnostic: same-host probe succeeded; main stream is the one that's stuck — \
             likely connection-specific (假说 D 进程内/单连接) or per-connection rate limit \
             upstream, NOT a network outage"
        );
    } else {
        // 探测也慢/失败 → 网络层确实有问题（假说 A 本地 / C 上游），
        // 不是单 stream 特异性。下一步看是不是 mission 同时拉了多个 stream（假说 B）。
        tracing::error!(
            provider = %provider_name,
            model = %model,
            probe_url = %outcome.url,
            probe_elapsed_ms = outcome.elapsed_ms,
            probe_status = outcome.status.unwrap_or(0),
            probe_error = %outcome.error.as_deref().unwrap_or(""),
            verdict = "NETWORK_LAYER_DEGRADED",
            "stall_diagnostic: same-host probe also slow/failed; network layer (local egress \
             or reseller upstream) is genuinely degraded — 假说 A/C confirmed for this incident"
        );
    }
}

/// 把全局 `StreamRegistry` 当前所有条目以一行一条的形式打到日志，给 stall 复盘看。
fn dump_active_streams_snapshot(
    reason: &str,
    idle_ms: u64,
    triggering_provider: &str,
    triggering_model: &str,
) {
    let snapshots: Vec<(u64, StatsSnapshot)> = StreamRegistry::global().snapshot_all();
    let active_count = snapshots.len();
    tracing::warn!(
        reason,
        idle_ms,
        active_streams = active_count,
        triggering_provider,
        triggering_model,
        "stall_diagnostic: active stream snapshot begin"
    );
    for (id, snap) in &snapshots {
        // 关键吞吐指标：bytes/s 用累计 bytes / running 时间近似（已经卡住的 stream
        // 这个值会被静默期摊得很低，正好暴露问题）。
        let bytes_per_sec = if snap.running_for_ms > 0 {
            (snap.total_bytes * 1000) / snap.running_for_ms
        } else {
            0
        };
        tracing::warn!(
            stream_id = id,
            provider = %snap.provider,
            model = %snap.model,
            endpoint = %snap.endpoint,
            running_for_ms = snap.running_for_ms,
            idle_ms = snap.idle_ms,
            total_bytes = snap.total_bytes,
            total_chunks = snap.total_chunks,
            text_chars = snap.text_chars,
            reasoning_chars = snap.reasoning_chars,
            bytes_per_sec,
            "stall_diagnostic: active stream"
        );
    }
    tracing::warn!(
        reason,
        active_streams = active_count,
        "stall_diagnostic: active stream snapshot end"
    );
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
            provider_extras: None,
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

        let response = handle
            .await
            .unwrap()
            .expect("downstream 关闭不应导致 helper 失败");
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

    /// `stall_diagnostic_trigger` 自适应：长阈值（生产 180s）→ 60s 触发；
    /// 短阈值（测试场景）→ 1/3 阈值触发，让短超时测试也能覆盖诊断路径。
    #[test]
    fn stall_diagnostic_trigger_scales() {
        // 生产场景：idle=180s → trigger=60s（const 上限）
        assert_eq!(
            stall_diagnostic_trigger(Duration::from_secs(180)),
            Duration::from_secs(60),
        );
        // 边界：idle=180s 是 60*3，刚好触到 const 上限
        assert_eq!(
            stall_diagnostic_trigger(Duration::from_secs(120)),
            Duration::from_secs(40),
            "中等阈值时应取 idle/3 而非 const，避免诊断永不触发"
        );
        // 短阈值（测试用）：idle=150ms → trigger=50ms
        assert_eq!(
            stall_diagnostic_trigger(Duration::from_millis(150)),
            Duration::from_millis(50),
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
