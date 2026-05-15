//! Coding Agent 执行引擎 (FM-15 Phase 3 重构版)。
//!
//! 关键变更（FR-09 / FR-11）：
//! - 完成检测从"无 tool_use 即完成"改为"必须调用 `task_complete` 工具"
//! - `task_complete` 触发 guardrails 顺序检查；失败则注入 user message 让 LLM 重试
//! - 重试预算耗尽 / 超时 / 步数超限 → 任务 failed（已修改文件仍 commit）
//! - 整个执行循环包裹 `tokio::time::timeout`，剩余 5 步时注入"请尽快收尾"提示
//!
//! 兼容性：当 task 没有任何 guardrails 配置时，guardrail run 仍会跑（结果为空 → 全部通过），
//! 等价于 Phase 2 行为；不会再因为 LLM "顺嘴说一句" 就误判完成。

use anyhow::Result;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::{Emitter, Manager};
use tokio::sync::mpsc;
use tokio::time::{timeout, Duration};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::db::{queries, Database};
use crate::llm::{
    stream_chat_with_idle_guard, ContentBlock, LlmProvider, LlmRequest, Message, MessageRole,
    StreamChunk, StreamChunkKind, StreamGuardError, DEFAULT_STREAM_IDLE_HEARTBEAT_SECS,
    DEFAULT_STREAM_IDLE_TIMEOUT,
};
use crate::tools::{coding_agent_tools_with_artifact_support, ToolExecutor, TASK_COMPLETE_TOOL};

use super::codebase_intel;
use super::guardrail::{self, Guardrail, GuardrailContext};
use super::types::*;

/// FR-11 默认值；Scheduler 可从配置覆盖。
///
/// 1800s（30 min）是为了配合 Phase 4 的"卡住才算超时"策略：LLM 流式响应有 stream-idle
/// 兜底（默认 60s 静默就杀），shell_exec 有进程级 watchdog（默认 60s idle / 5min wall），
/// 这层 wall-clock 仅作为兜底防御无限循环；正常任务远到不了。
pub const DEFAULT_AGENT_TIMEOUT_SECS: u64 = 1800;
pub const DEFAULT_MAX_AGENT_STEPS: u32 = 80;
/// 当 LLM 连续 N 次不调用任何工具但又没调用 task_complete，就注入提示。
const MAX_CONSECUTIVE_NO_TOOL: u32 = 3;
/// L3 循环检测：连续 N 步只调用只读工具（read/search/list）就注入"开始动手"提示。
const READ_ONLY_LOOP_THRESHOLD: u32 = 5;
/// 步数距上限只剩 N 时注入"剩余 N 步"提示。
const STEPS_REMAINING_HINT: u32 = 5;
/// Issue 3: 单步 LLM 流被 idle watchdog 中止（卡住 180s）时，给 LLM 发"continue"
/// 重试的次数预算。耗尽则真失败。**Step 级**：每个 step 开始时重置。
///
/// 之前一次 IdleTimeout 就把整个 agent 标 failed，对于经常半截卡住的 reseller
/// （DeepSeek-V4 / SiliconFlow Qwen）非常痛。改成"卡住就 continue"，更接近用户
/// 在 Cursor / Claude Desktop 看到 "continue" 按钮的直觉。
///
/// 为什么 step 级而非任务级：任务级 budget 等于把"可恢复故障"当"不可恢复故障"
/// 处理——一个 80-step 任务里偶发 3 次卡就直接 failed，违背 retry 的本意。
/// max_steps 自身（80）已是 retry 总次数的隐式上限，加上 cancel_token + UI 可见
/// 的 system_hint，无需再加任务级 budget。
const DEFAULT_IDLE_RETRY_BUDGET: u32 = 2;

/// Issue 3: 纯函数版的"idle-retry budget 转移"语义。
///
/// 抽出来仅是为了写单测——loop 里实际还是 inline 状态机。**契约**：
/// - 进入新 step（`resume_after_idle_retry == false`）→ budget 重置到 `default`
/// - 上一次是 retry 跳过来的（`resume_after_idle_retry == true`）→ 保留当前 budget
///
/// 任何对这个函数的"简化"（例如忘了 reset 或永远 reset）都会被下面 mod tests 抓住。
#[inline]
fn next_idle_retry_budget(
    resume_after_idle_retry: bool,
    current: u32,
    default: u32,
) -> u32 {
    if resume_after_idle_retry {
        current
    } else {
        default
    }
}

/// 只读工具集合（不会改变工作区状态）。L3 循环检测据此判断是否在原地探索。
fn is_read_only_tool(name: &str) -> bool {
    matches!(name, "read_file" | "search_files" | "list_files")
}

// ---- Single-Agent Uplift Phase 2.2: 上下文瘦身 ----

/// 单个 tool_result 内容超过 8KB chars 就触发截断。
/// 之所以用 chars 而非 bytes：tokenizer 对字符敏感而非字节，且 8KB chars ≈ 2K tokens
/// ——一条 result 占 2K tokens 已经是"开始挤垮 prompt"的临界值。
const TOOL_RESULT_BUDGET_CHARS: usize = 8 * 1024;
/// 截断后保留尾部 N chars——尾部往往是错误堆栈 / 最终输出，比头部更重要。
const TOOL_RESULT_TAIL_CHARS: usize = 1024;
/// "已经截过的"哨兵串前缀，避免重复截断把 sentinel 自己当原内容再截一次。
const TRUNCATED_SENTINEL_PREFIX: &str = "[result truncated to keep context lean.";
/// 整个 prompt 的 token 预算（粗估）。超过就 microcompact。
/// 50K 是大多数 chat completion 模型 (Claude / GPT-4o / DeepSeek) 安全区的下沿。
const MICROCOMPACT_TOKEN_THRESHOLD: usize = 50_000;
/// chars-to-tokens 粗估常数。代码 / JSON 大约 3.5 chars/token，取 4 偏保守
/// (略高估 → 提前触发压缩，宁可早一步也别超 ctx)。
const CHARS_PER_TOKEN_ESTIMATE: usize = 4;

/// 把 messages 里所有 ToolResult 的 content 截短到 TOOL_RESULT_BUDGET_CHARS 以内。
/// 对已经截过的 result 幂等（看 sentinel 前缀）。
///
/// 为什么 in-place 改：之前以为前端展开按钮要看原文，所以不能动 messages。
/// 实际原文已经在 agent_events.content 里持久化（emit_event_with_meta 走的是原 ToolOutput.content），
/// 改 messages 只影响下次 LLM 请求 —— 用户视角无感。
fn apply_tool_result_budget(messages: &mut [Message]) {
    for msg in messages.iter_mut() {
        for block in msg.content.iter_mut() {
            if let ContentBlock::ToolResult { content, .. } = block {
                if content.starts_with(TRUNCATED_SENTINEL_PREFIX) {
                    continue;
                }
                let total = content.chars().count();
                if total <= TOOL_RESULT_BUDGET_CHARS {
                    continue;
                }
                let tail: String = content
                    .chars()
                    .skip(total - TOOL_RESULT_TAIL_CHARS)
                    .collect();
                let total_kb = total / 1024;
                *content = format!(
                    "{TRUNCATED_SENTINEL_PREFIX} Original size: {total_kb}KB. Last {} chars:\n{tail}]",
                    TOOL_RESULT_TAIL_CHARS,
                );
            }
        }
    }
}

/// 用 chars / 4 粗估 prompt 的 token 数。包含 system 之外的 messages（system 一般固定，
/// microcompact 不动它）。仅作为触发阈值，不要求精确。
fn approximate_tokens(messages: &[Message]) -> usize {
    let mut chars = 0usize;
    for msg in messages {
        for block in &msg.content {
            chars += match block {
                ContentBlock::Text { text } => text.len(),
                ContentBlock::Reasoning { text } => text.len(),
                ContentBlock::ToolUse { input, .. } => {
                    serde_json::to_string(input).map(|s| s.len()).unwrap_or(0)
                }
                ContentBlock::ToolResult { content, .. } => content.len(),
            };
        }
    }
    chars / CHARS_PER_TOKEN_ESTIMATE
}

#[derive(Debug, Clone)]
struct CompactReport {
    dropped_messages: usize,
    tools_seen: Vec<String>,
    tokens_before: usize,
    tokens_after: usize,
}

impl CompactReport {
    fn human_readable(&self) -> String {
        format!(
            "Compacted {} earlier message(s) to free context. ~{}K → ~{}K tokens. \
             Earlier tool calls: {}.",
            self.dropped_messages,
            self.tokens_before / 1000,
            self.tokens_after / 1000,
            if self.tools_seen.is_empty() {
                "(none)".to_string()
            } else {
                self.tools_seen.join(", ")
            },
        )
    }

    fn to_meta(&self) -> serde_json::Value {
        serde_json::json!({
            "dropped_messages": self.dropped_messages,
            "tokens_before": self.tokens_before,
            "tokens_after": self.tokens_after,
            "tools_seen": self.tools_seen,
        })
    }
}

/// 把最早 ~1/3 messages 折叠成一条 "earlier explored: ..." 摘要，整体压缩。
///
/// 设计考虑：
/// - 不动 system prompt（caller 把它放在 LlmRequest::system，不在 messages 里）
/// - 不动最近 ⅔ messages —— 通常 LLM 当前正在看的上下文都在尾部，截尾就直接退化
/// - 折叠出来的摘要插在最前面（user role）—— LLM 看到一段历史综述比直接断片更不会乱
/// - 只在 messages 数 ≥ 8 时才动手，太少消息折叠收益小且容易丢上下文
/// - 返回 `None` 表示什么也没做（caller 别 emit `compact` 事件）
fn microcompact(messages: &mut Vec<Message>) -> Option<CompactReport> {
    if messages.len() < 8 {
        return None;
    }
    let before = approximate_tokens(messages);
    let drop_count = messages.len() / 3;
    let dropped: Vec<Message> = messages.drain(0..drop_count).collect();

    // 从被丢的 messages 里抽出工具名做 summary —— "你之前用了哪些工具" 比 "你之前讲了啥"
    // 更能帮 LLM 判断"还需不需要重做某事"。
    let mut tools_seen: Vec<String> = Vec::new();
    for msg in &dropped {
        for block in &msg.content {
            if let ContentBlock::ToolUse { name, .. } = block {
                if !tools_seen.contains(name) {
                    tools_seen.push(name.clone());
                }
            }
        }
    }

    let summary = format!(
        "[context-compact] {} earlier message(s) have been compacted to keep the prompt small. \
         The full event history is still visible to the user in the workspace timeline. \
         Tools you ran earlier: {}. Continue from the latest user/tool messages below.",
        drop_count,
        if tools_seen.is_empty() {
            "(none)".to_string()
        } else {
            tools_seen.join(", ")
        },
    );

    messages.insert(
        0,
        Message {
            role: MessageRole::User,
            content: vec![ContentBlock::Text { text: summary }],
            cache_control: None,
        },
    );

    let after = approximate_tokens(messages);
    Some(CompactReport {
        dropped_messages: drop_count,
        tools_seen,
        tokens_before: before,
        tokens_after: after,
    })
}

#[derive(Debug, Clone, serde::Serialize)]
struct AgentEventPayload {
    agent_id: String,
    step: u32,
    kind: String,
    content: String,
    /// Single-Agent Uplift Phase 0.2: 结构化 payload。前端按 kind 解析渲染（diff、todo
    /// 列表、guardrail report）。`None` 表示纯文本事件，前端走 fallback 行为。
    #[serde(skip_serializing_if = "Option::is_none")]
    meta: Option<serde_json::Value>,
}

/// AgentEngine 运行时配置（FR-09 / FR-11）。
pub struct AgentRunOptions {
    pub model: String,
    pub max_steps: u32,
    pub timeout_secs: u64,
    pub guardrails: Vec<Guardrail>,
    pub guardrail_retry_budget: u32,
    /// 来自 task.produces_artifacts 解析后的 (local_name, type) 对，供 ArtifactsExist guardrail 使用。
    pub produces: Vec<(String, String)>,
    pub expected_output: Option<String>,
    /// Issue 3: stream idle timeout 时给 LLM 发"continue"重试的次数预算。
    pub idle_retry_budget: u32,
}

impl Default for AgentRunOptions {
    fn default() -> Self {
        Self {
            model: String::new(),
            max_steps: DEFAULT_MAX_AGENT_STEPS,
            timeout_secs: DEFAULT_AGENT_TIMEOUT_SECS,
            guardrails: Vec::new(),
            guardrail_retry_budget: 3,
            produces: Vec::new(),
            expected_output: None,
            idle_retry_budget: DEFAULT_IDLE_RETRY_BUDGET,
        }
    }
}

pub struct AgentEngine {
    provider: Arc<dyn LlmProvider>,
    tool_executor: ToolExecutor,
    workspace_root: PathBuf,
    app_handle: tauri::AppHandle,
    cancel_token: CancellationToken,
}

impl AgentEngine {
    pub fn new(
        provider: Arc<dyn LlmProvider>,
        workspace_root: PathBuf,
        app_handle: tauri::AppHandle,
        cancel_token: CancellationToken,
    ) -> Self {
        Self {
            provider,
            tool_executor: ToolExecutor::new(workspace_root.clone()),
            workspace_root,
            app_handle,
            cancel_token,
        }
    }

    /// 兼容旧调用点：保留旧签名（max_steps），其它走 Default。
    /// FM-15 Phase 3 后续应迁移到 `run_with_options`。
    pub async fn run(
        &self,
        agent_id: &str,
        task_description: &str,
        model: &str,
        max_steps: u32,
    ) -> Result<AgentStatus> {
        let opts = AgentRunOptions {
            model: model.to_string(),
            max_steps: if max_steps == 0 || max_steps == u32::MAX {
                DEFAULT_MAX_AGENT_STEPS
            } else {
                max_steps
            },
            ..AgentRunOptions::default()
        };
        self.run_with_options(agent_id, task_description, &opts).await
    }

    /// FM-15 Phase 3 主入口：携带 guardrail / timeout / max_steps 配置完整运行 Coding Agent。
    pub async fn run_with_options(
        &self,
        agent_id: &str,
        task_description: &str,
        opts: &AgentRunOptions,
    ) -> Result<AgentStatus> {
        let outer_dur = Duration::from_secs(opts.timeout_secs.max(1));
        match timeout(outer_dur, self.run_inner(agent_id, task_description, opts)).await {
            Ok(res) => res,
            Err(_) => {
                tracing::warn!(
                    "Agent {agent_id} hit wall-clock timeout ({:?}); marking failed",
                    outer_dur
                );
                let reason = format!("timeout: wall_clock {}s exceeded", opts.timeout_secs);
                self.emit_event(agent_id, 0, "error", &reason);
                self.emit_event(agent_id, 0, "status_change", "failed");
                self.update_agent_status(agent_id, "failed");
                self.expire_agent_notes(agent_id);
                self.mark_task_failed_with_reason(agent_id, "failed", &reason);
                Ok(AgentStatus::Failed)
            }
        }
    }

    async fn run_inner(
        &self,
        agent_id: &str,
        task_description: &str,
        opts: &AgentRunOptions,
    ) -> Result<AgentStatus> {
        let tools = coding_agent_tools_with_artifact_support();
        let workspace_dir = self.tool_executor.workspace_display();
        let guardrail_brief = render_guardrail_brief(&opts.guardrails);
        let produces_brief = render_produces_brief(&opts.produces);
        let expected_brief = opts
            .expected_output
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .map(|s| format!("\n\n## Expected Output\n{s}"))
            .unwrap_or_default();

        // FR-10: Codebase Intelligence —— 注入 [Project Structure] / [Tech Stack] /
        // [Upstream Context] / [Base Conflicts]。task_id 通过 agent_id 反查；任何步骤失败则
        // 该块为空，不阻塞 agent 启动。
        let db_state = self.app_handle.state::<Database>();
        let task_id_for_intel: Option<String> = db_state
            .with_conn(|conn| queries::get_task_id_for_agent(conn, agent_id))
            .ok()
            .flatten();
        let intel = codebase_intel::build_intel(
            &self.workspace_root,
            task_id_for_intel.as_deref(),
            Some(&db_state),
        );
        let intel_block = intel.render_system_block();

        let system = format!(
            "You are a coding agent working in the directory: {workspace_dir}\n\n\
             ## Task\n{task_description}{expected_brief}{produces_brief}\n\n\
             ## Tools & Completion Protocol\n\
             - Use the provided tools to explore, read, write, and search files.\n\
             - All file paths are relative to the workspace root.\n\
             - When you have finished implementing the task and saved all files, you MUST call \
             the `task_complete` tool with a concise summary. \
             Do NOT just write a textual summary — only `task_complete` ends the task.\n\
             - Before calling `task_complete`, publish every artifact that was planned for this \
             task using `publish_artifact` (file_paths must point to files that already exist on disk).{guardrail_brief}\n\
             - ALWAYS provide all required parameters when calling a tool.{intel_block}"
        );

        let mut messages: Vec<Message> = Vec::new();
        let mut step: u32 = 0;
        let mut consecutive_no_tool: u32 = 0;
        let mut consecutive_read_only: u32 = 0;
        let mut hinted_read_only_loop = false;
        let mut retries_left: u32 = opts.guardrail_retry_budget;
        let mut hinted_remaining_steps = false;
        // Issue 3: idle-retry 预算（**step 级**）。
        //
        // 早期设计是"任务级"——整个 task 总共 2 次容错——结果一个 80-step 的任务
        // 偶发卡 3 次就直接 failed。Reseller 的真实卡频率（DeepSeek-V4 / SiliconFlow
        // Qwen）大约是每 10-20 step 一次，任务级 budget 等于把可恢复故障当不可恢复
        // 处理，违背 retry 的本意。
        //
        // 改成 step 级：每个 step 开始时把 budget 重置到 `opts.idle_retry_budget`。
        // 兜底 invariant：
        // - `max_steps`（默认 80）天然是 retry 总次数的隐式上限（每次 retry 都 step += 1）
        // - 每次 retry 都 emit `system_hint`，用户能在 Workspace 实时看到，可主动 cancel
        // - 与"每 step 是一次独立 LLM 调用"的语义对齐，更符合直觉
        let mut idle_retries_left: u32 = opts.idle_retry_budget;
        let mut resume_after_idle_retry = false;

        self.emit_event(agent_id, step, "status_change", "running");
        self.update_agent_status(agent_id, "running");

        loop {
            // step 级 budget 重置：仅当本次迭代不是从 idle-retry continue 跳过来时
            // 才重置。语义已抽到 `next_idle_retry_budget` 单独写测试守住。
            idle_retries_left = next_idle_retry_budget(
                resume_after_idle_retry,
                idle_retries_left,
                opts.idle_retry_budget,
            );
            resume_after_idle_retry = false;

            if self.cancel_token.is_cancelled() {
                return self.finish_cancelled(agent_id, step);
            }

            if step >= opts.max_steps {
                let reason = format!(
                    "max_steps: {} steps exhausted without task_complete",
                    opts.max_steps
                );
                self.emit_event(agent_id, step, "error", &reason);
                self.emit_event(agent_id, step, "status_change", "failed");
                self.update_agent_status(agent_id, "failed");
                self.expire_agent_notes(agent_id);
                self.mark_task_failed_with_reason(agent_id, "failed", &reason);
                return Ok(AgentStatus::Failed);
            }

            // 剩余步数 ≤ STEPS_REMAINING_HINT 时注入一条提示（一次性）
            if !hinted_remaining_steps
                && opts.max_steps > STEPS_REMAINING_HINT
                && opts.max_steps - step <= STEPS_REMAINING_HINT
            {
                hinted_remaining_steps = true;
                let hint = format!(
                    "[System] You have only {} steps left. Wrap up your work and call \
                     task_complete soon.",
                    opts.max_steps - step
                );
                messages.push(Message {
                    role: MessageRole::User,
                    content: vec![ContentBlock::Text { text: hint.clone() }],
                    cache_control: None,
                });
                self.emit_event(agent_id, step, "system_hint", &hint);
            }

            step += 1;
            self.update_agent_step(agent_id, step);

            // Single-Agent Uplift Phase 2.2: prompt 进 LLM 之前先做两层瘦身。
            //   ① tool_result 大单元截断（>8KB chars 替换成头尾 sentinel）—— DB 仍存原文
            //   ② 整体 token 估算超 50K → microcompact，丢最早 1/3 messages 换一段 summary
            // 触发任一动作都 emit `compact` 事件让用户知情，避免 LLM 行为突变无解释。
            apply_tool_result_budget(&mut messages);
            if approximate_tokens(&messages) > MICROCOMPACT_TOKEN_THRESHOLD {
                if let Some(report) = microcompact(&mut messages) {
                    self.emit_event_with_meta(
                        agent_id,
                        step,
                        "compact",
                        &report.human_readable(),
                        Some(report.to_meta()),
                    );
                }
            }

            let call_summary = Self::describe_llm_call(step, &messages);
            self.emit_event(agent_id, step, "llm_call", &call_summary);

            let request = LlmRequest {
                model: opts.model.clone(),
                system: Some(system.clone()),
                messages: messages.clone(),
                tools: tools.clone(),
                max_tokens: 4096,
            };

            let (tx, mut rx) = mpsc::channel::<StreamChunk>(256);
            let agent_id_owned = agent_id.to_string();
            let app_handle = self.app_handle.clone();
            let stream_step = step;
            // Single-Agent Uplift Phase 0.4: shared 活动计时器。
            // 用 AtomicU64 存自 step 开始以来的毫秒数；每收到一个 chunk 更新它。
            // heartbeat 任务读它来判断是否进入"看起来卡住"状态。
            let step_started_at = std::time::Instant::now();
            let last_chunk_at = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
            let last_chunk_at_fwd = last_chunk_at.clone();
            let forwarder = tokio::spawn(async move {
                while let Some(chunk) = rx.recv().await {
                    last_chunk_at_fwd.store(
                        step_started_at.elapsed().as_millis() as u64,
                        std::sync::atomic::Ordering::Relaxed,
                    );
                    if let StreamChunkKind::TextDelta = chunk.kind {
                        let _ = app_handle.emit(
                            "agent-stream",
                            AgentEventPayload {
                                agent_id: agent_id_owned.clone(),
                                step: stream_step,
                                kind: "text_delta".to_string(),
                                content: chunk.content,
                                meta: None,
                            },
                        );
                    }
                }
            });

            // Single-Agent Uplift Phase 0.4: Heartbeat。
            // LLM stream 静默 ≥ HEARTBEAT_IDLE_SECS 时往前端推一条 tool_progress，
            // 让用户实时看到"⏳ 等待 LLM 已 30s..."而不是干瞪着空 terminal。
            //
            // 抢先于 stream_chat_with_idle_guard 的 180s abort —— 这条事件不杀流，
            // 只是知会用户。状态的真正终止仍由 idle_guard 决定。
            let heartbeat_cancel = std::sync::Arc::new(tokio::sync::Notify::new());
            let heartbeat_cancel_for_task = heartbeat_cancel.clone();
            let heartbeat_app = self.app_handle.clone();
            let heartbeat_agent_id = agent_id.to_string();
            let heartbeat_step = step;
            let heartbeat_last_chunk_at = last_chunk_at.clone();
            let heartbeat_handle = tokio::spawn(async move {
                /// 静默多少秒触发第一次 heartbeat。从 llm::stream_guard 单源取值，
                /// 避免两边阈值漂移。30s 是用户开始疑神疑鬼的临界点。
                const HEARTBEAT_IDLE_SECS: u64 = DEFAULT_STREAM_IDLE_HEARTBEAT_SECS;
                /// 之后每 N 秒再推一次。频率太高刷屏，太低不及时。
                const HEARTBEAT_REPEAT_SECS: u64 = 15;
                let mut last_emitted_at_ms: Option<u64> = None;
                loop {
                    tokio::select! {
                        _ = heartbeat_cancel_for_task.notified() => break,
                        _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => {
                            let now_ms = step_started_at.elapsed().as_millis() as u64;
                            let last_ms = heartbeat_last_chunk_at.load(std::sync::atomic::Ordering::Relaxed);
                            let idle_ms = now_ms.saturating_sub(last_ms);
                            let idle_secs = idle_ms / 1000;
                            if idle_secs < HEARTBEAT_IDLE_SECS {
                                continue;
                            }
                            // 距离上次 emit 不到 HEARTBEAT_REPEAT_SECS 时不重复
                            if let Some(prev) = last_emitted_at_ms {
                                if now_ms.saturating_sub(prev) < HEARTBEAT_REPEAT_SECS * 1000 {
                                    continue;
                                }
                            }
                            last_emitted_at_ms = Some(now_ms);
                            let content = format!(
                                "Waiting for LLM... idle for {idle_secs}s",
                            );
                            let meta = serde_json::json!({
                                "kind": "llm_idle",
                                "idle_secs": idle_secs,
                                "step": heartbeat_step,
                            });
                            // heartbeat 只 emit 推送，不持久化 —— 持久化后每个 step 都
                            // 会留一堆"还在等"事件，污染 timeline。前端"实时"足矣。
                            let _ = heartbeat_app.emit(
                                "agent-event",
                                AgentEventPayload {
                                    agent_id: heartbeat_agent_id.clone(),
                                    step: heartbeat_step,
                                    kind: "tool_progress".to_string(),
                                    content,
                                    meta: Some(meta),
                                },
                            );
                        }
                    }
                }
            });

            // Idle 看门狗统一走 llm::stream_guard：长沉默 abort，避免 agent
            // 单步永远卡死整个任务（之前完全没有空闲保护）。
            //
            // Issue 3: IdleTimeout 不再立即 fail —— 还有 idle_retries_left 时注入
            // 一条 "[System] 上一次响应中断，请继续" 的 user 提示，下一次 loop
            // 重新发起 LLM 调用。这模拟用户在 Cursor / Claude Desktop 按 "continue"
            // 的体验，对偶发卡住的 reseller（DeepSeek-V4 / SiliconFlow Qwen）尤其有效。
            // 其他错误（Llm / Join）保持原失败路径。
            let stream_outcome = stream_chat_with_idle_guard(
                self.provider.clone(),
                request,
                tx,
                DEFAULT_STREAM_IDLE_TIMEOUT,
            )
            .await;
            let _ = forwarder.await;
            heartbeat_cancel.notify_waiters();
            let _ = heartbeat_handle.await;
            let response = match stream_outcome {
                Ok(r) => r,
                Err(StreamGuardError::IdleTimeout { idle_secs, threshold_secs })
                    if idle_retries_left > 0 =>
                {
                    idle_retries_left -= 1;
                    // 关键：标记下一次 loop 是"延续本 step 的 retry"，否则 loop 顶部
                    // 会把 budget 重置回满，等于无限 retry。
                    resume_after_idle_retry = true;
                    let notice = format!(
                        "LLM stream idle for {idle_secs}s (threshold {threshold_secs}s); auto-continue ({} retries left this step)",
                        idle_retries_left
                    );
                    tracing::warn!(
                        agent_id = %agent_id,
                        step,
                        idle_secs,
                        threshold_secs,
                        retries_left = idle_retries_left,
                        "stream idle timeout, auto-injecting continue prompt"
                    );
                    self.emit_event(agent_id, step, "system_hint", &notice);
                    // 没有 assistant turn 可 push（流被中止）。直接追加一条 user
                    // 提示给 LLM 让它在下一次 stream 里基于已有上下文继续。
                    let continue_msg = format!(
                        "[System] 上一次响应在 {idle_secs}s 后中断未输出完整内容。请基于到目前为止的对话上下文继续完成任务；\
                         不需要重复你已经说过的内容，直接接着写。如果上次正打算调用工具，请重新调用一次。"
                    );
                    messages.push(Message {
                        role: MessageRole::User,
                        content: vec![ContentBlock::Text { text: continue_msg }],
                        cache_control: None,
                    });
                    continue;
                }
                Err(e) => return Err(anyhow::anyhow!(e.user_message_zh())),
            };

            if self.cancel_token.is_cancelled() {
                return self.finish_cancelled(agent_id, step);
            }

            messages.push(Message {
                role: MessageRole::Assistant,
                content: response.content.clone(),
                cache_control: None,
            });

            let step_cost = self.provider.estimate_cost(
                &opts.model,
                response.usage.input_tokens,
                response.usage.output_tokens,
            );
            self.persist_cost_record(
                agent_id,
                &opts.model,
                response.usage.input_tokens,
                response.usage.output_tokens,
                step_cost,
            );
            self.accumulate_agent_cost(
                agent_id,
                response.usage.input_tokens,
                response.usage.output_tokens,
                step_cost,
            );

            // FM-14: budget gate —— 累计成本触线时阻塞当前 agent 等待审批。
            // rejected → 标 task failed 让 mission 自然走完终态判定。
            // approved / 触发不到（ratio=0 或未签 contract）→ 静默继续。
            if let Some(coord) = self
                .app_handle
                .try_state::<std::sync::Arc<crate::agent::approval::ApprovalCoordinator>>()
            {
                let db = self.app_handle.state::<Database>();
                let mission_id_opt: Option<String> = db
                    .with_conn(|conn| queries::get_mission_id_for_agent(conn, agent_id))
                    .ok()
                    .flatten();
                if let Some(mission_id) = mission_id_opt {
                    use crate::agent::approval::ApprovalDecision;
                    use crate::agent::approval_gate::maybe_trigger_budget;
                    if let Some(decision) = maybe_trigger_budget(
                        &self.app_handle,
                        coord.inner(),
                        db.inner(),
                        &self.cancel_token,
                        &mission_id,
                        agent_id,
                    )
                    .await
                    {
                        if matches!(
                            decision,
                            ApprovalDecision::Rejected | ApprovalDecision::Cancelled
                        ) {
                            let reason = "budget: user rejected continuation past warn threshold";
                            self.emit_event(agent_id, step, "error", reason);
                            self.emit_event(agent_id, step, "status_change", "failed");
                            self.update_agent_status(agent_id, "failed");
                            self.expire_agent_notes(agent_id);
                            self.mark_task_failed_with_reason(agent_id, "failed", reason);
                            return Ok(AgentStatus::Failed);
                        }
                    }
                }
            }

            self.emit_event(
                agent_id,
                step,
                "checkpoint",
                &format!(
                    "tokens: {}in/{}out | cost: ${:.4} | stop: {}",
                    response.usage.input_tokens,
                    response.usage.output_tokens,
                    step_cost,
                    response.stop_reason
                ),
            );

            // 判断本步是否调用了任何工具，以及是否调用了 task_complete
            let mut tool_use_blocks: Vec<(String, String, serde_json::Value)> = Vec::new();
            for block in &response.content {
                if let ContentBlock::ToolUse { id, name, input } = block {
                    tool_use_blocks.push((id.clone(), name.clone(), input.clone()));
                }
            }
            let task_complete_call = tool_use_blocks
                .iter()
                .find(|(_, name, _)| name == TASK_COMPLETE_TOOL);

            if let Some((_, _, input)) = task_complete_call.cloned() {
                // FR-09.3-5: 跑 guardrails，决定是 Completed 还是注入失败重试
                let summary = input
                    .get("summary")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                self.emit_event_with_meta(
                    agent_id,
                    step,
                    "tool_use",
                    &format!("task_complete({{\"summary\": ...}})"),
                    Some(serde_json::json!({
                        "tool": TASK_COMPLETE_TOOL,
                        "input": { "summary": &summary },
                    })),
                );

                let outcome = self
                    .evaluate_completion(agent_id, step, &summary, opts)
                    .await;
                match outcome {
                    CompletionOutcome::Completed => {
                        self.emit_event(agent_id, step, "message", &summary);
                        self.persist_completion_summary(agent_id, &summary);
                        self.emit_event(agent_id, step, "status_change", "completed");
                        self.update_agent_status(agent_id, "completed");
                        self.expire_agent_notes(agent_id);
                        return Ok(AgentStatus::Completed);
                    }
                    CompletionOutcome::Retry { feedback } => {
                        if retries_left == 0 {
                            let reason = format!(
                                "guardrail: retry budget exhausted ({}); last_feedback={}",
                                opts.guardrail_retry_budget,
                                feedback.chars().take(160).collect::<String>()
                            );
                            self.emit_event(agent_id, step, "error", &reason);
                            self.emit_event(agent_id, step, "status_change", "failed");
                            self.update_agent_status(agent_id, "failed");
                            self.expire_agent_notes(agent_id);
                            self.mark_task_failed_with_reason(agent_id, "failed", &reason);
                            return Ok(AgentStatus::Failed);
                        }
                        retries_left -= 1;
                        let mut tool_results: Vec<ContentBlock> = Vec::new();
                        // 把 task_complete 工具回执填回（避免破坏 OpenAI tool_use 配对）
                        for (id, name, _) in &tool_use_blocks {
                            if name == TASK_COMPLETE_TOOL {
                                tool_results.push(ContentBlock::ToolResult {
                                    tool_use_id: id.clone(),
                                    content: feedback.clone(),
                                    is_error: true,
                                });
                            }
                        }
                        // 其它工具调用仍然要按正常流程执行（不太常见，但为完整性）
                        for (id, name, input) in &tool_use_blocks {
                            if name == TASK_COMPLETE_TOOL {
                                continue;
                            }
                            let output = self
                                .tool_executor
                                .execute_with_stream(name, input, &self.app_handle, agent_id)
                                .await;
                            tool_results.push(ContentBlock::ToolResult {
                                tool_use_id: id.clone(),
                                content: output.content,
                                is_error: output.is_error,
                            });
                        }
                        messages.push(Message {
                            role: MessageRole::User,
                            content: tool_results,
                            cache_control: None,
                        });
                        consecutive_no_tool = 0;
                        continue;
                    }
                }
            }

            // 没有 task_complete：处理"普通工具调用 / 无工具"两种情况
            let has_any_tool_use = !tool_use_blocks.is_empty();
            if !has_any_tool_use {
                consecutive_no_tool += 1;
                if consecutive_no_tool >= MAX_CONSECUTIVE_NO_TOOL {
                    let hint = format!(
                        "[System] You have produced {} replies without using any tool. \
                         Either continue with a tool call or signal completion via the \
                         `task_complete` tool. The task is NOT considered complete until \
                         `task_complete` succeeds.",
                        consecutive_no_tool
                    );
                    messages.push(Message {
                        role: MessageRole::User,
                        content: vec![ContentBlock::Text { text: hint.clone() }],
                        cache_control: None,
                    });
                    self.emit_event(agent_id, step, "system_hint", &hint);
                }
                continue;
            }
            consecutive_no_tool = 0;

            // L3 循环检测：连续 N 步只调用只读工具（read/search/list）→ 注入"开始动手"提示，
            // 帮 LLM 跳出"光读不写"的死循环。一次性，避免重复打扰。
            let all_read_only = tool_use_blocks
                .iter()
                .all(|(_, name, _)| is_read_only_tool(name));
            if all_read_only {
                consecutive_read_only += 1;
            } else {
                consecutive_read_only = 0;
                hinted_read_only_loop = false;
            }
            if !hinted_read_only_loop && consecutive_read_only >= READ_ONLY_LOOP_THRESHOLD {
                hinted_read_only_loop = true;
                let hint = format!(
                    "[System] You have spent {} consecutive steps only reading / searching files \
                     without making any change. Either start writing (`write_file`), running a \
                     command (`shell_exec`), or — if exploration is finished — call \
                     `task_complete`. Endless exploration is treated as a failure.",
                    consecutive_read_only
                );
                self.emit_event(agent_id, step, "system_hint", &hint);
                messages.push(Message {
                    role: MessageRole::User,
                    content: vec![ContentBlock::Text { text: hint }],
                    cache_control: None,
                });
            }

            // Single-Agent Uplift Phase 2.1: 并发安全的工具批量并行执行。
            //
            // 之前所有 tool_use 严格串行 → 一个 step 跑 3 个 read_file 等于 3× IO 延迟。
            // 现在按 ToolSpec.is_concurrency_safe 分桶：
            //   - safe  (read_file / list_files / search_files / glob): 并行跑
            //   - unsafe(write_file / edit_file / shell_exec / publish_artifact /
            //            todo_write): 串行跑（防写盘冲突 / approval gate 顺序错乱）
            //
            // tool_use 事件全部前置一次性 emit，让用户立即看到"这一批要做这些"；
            // tool_result 事件在每个 future 完成时 emit（顺序按完成时间，不按 tool_use_blocks
            // 顺序），用户能感知到"X 已经回来了，Y 还在跑"。
            //
            // tool_results vec 仍按原顺序填回 messages，因为 Anthropic 要求 ToolResult
            // 序与同 turn 的 ToolUse 严格一一对应。
            //
            // 注意：approval_gate 只拦截 unsafe 工具（write 类 / shell 类），所以 safe
            // 桶不会因为 approval 等待造成相互阻塞 → 真正能并行。
            for (id, name, input) in &tool_use_blocks {
                let tool_use_meta = serde_json::json!({
                    "tool": name,
                    "tool_use_id": id,
                    "input": input,
                });
                self.emit_event_with_meta(
                    agent_id,
                    step,
                    "tool_use",
                    &format!(
                        "{name}({})",
                        serde_json::to_string(input).unwrap_or_default()
                    ),
                    Some(tool_use_meta),
                );
            }

            // 计算每个 block 是否 concurrency-safe。未知工具（registry 没注册的）
            // 默认按 unsafe 处理——保守。task_complete / publish_artifact / todo_write
            // 不在 registry，自然落 unsafe。
            let safe_flags: Vec<bool> = tool_use_blocks
                .iter()
                .map(|(_, name, _)| {
                    crate::tools::lookup_tool_spec(name)
                        .map(|s| s.is_concurrency_safe)
                        .unwrap_or(false)
                })
                .collect();

            // 预分配结果 slot；按原 index 填回。Vec<Option<...>> 是常见的"并行下沉
            // 后保持顺序"模式，比 HashMap 省一次哈希且 cache friendly。
            let mut tool_outputs: Vec<Option<crate::tools::ToolOutput>> =
                (0..tool_use_blocks.len()).map(|_| None).collect();

            // 1) 并行跑 safe 桶。futures::future::join_all 不要求 'static，
            //    每个 future 借 &self，到 .await 结束借用归还。
            let safe_futures: Vec<_> = tool_use_blocks
                .iter()
                .enumerate()
                .filter_map(|(i, blk)| {
                    if safe_flags[i] {
                        let (id, name, input) = blk;
                        Some(async move {
                            let started_at = std::time::Instant::now();
                            let output =
                                self.dispatch_tool(agent_id, name, input).await;
                            let duration_ms = started_at.elapsed().as_millis() as u64;
                            (i, id.clone(), name.clone(), output, duration_ms)
                        })
                    } else {
                        None
                    }
                })
                .collect();
            if !safe_futures.is_empty() {
                let results = futures::future::join_all(safe_futures).await;
                for (i, id, name, output, duration_ms) in results {
                    let event_kind = if output.is_error { "error" } else { "tool_result" };
                    let result_meta = serde_json::json!({
                        "tool": name,
                        "tool_use_id": id,
                        "is_error": output.is_error,
                        "duration_ms": duration_ms,
                        "size_chars": output.content.chars().count(),
                        "concurrent": true,
                    });
                    self.emit_event_with_meta(
                        agent_id,
                        step,
                        event_kind,
                        &output.content,
                        Some(result_meta),
                    );
                    tool_outputs[i] = Some(output);
                }
            }

            // 2) 串行跑 unsafe 桶。保留原 index 顺序。
            for (i, blk) in tool_use_blocks.iter().enumerate() {
                if safe_flags[i] {
                    continue;
                }
                let (id, name, input) = blk;
                let started_at = std::time::Instant::now();
                let output = self.dispatch_tool(agent_id, name, input).await;
                let duration_ms = started_at.elapsed().as_millis() as u64;
                let event_kind = if output.is_error { "error" } else { "tool_result" };
                let result_meta = serde_json::json!({
                    "tool": name,
                    "tool_use_id": id,
                    "is_error": output.is_error,
                    "duration_ms": duration_ms,
                    "size_chars": output.content.chars().count(),
                    "concurrent": false,
                });
                self.emit_event_with_meta(
                    agent_id,
                    step,
                    event_kind,
                    &output.content,
                    Some(result_meta),
                );
                tool_outputs[i] = Some(output);
            }

            // 3) 按原顺序拼 tool_results。Anthropic 要求 ToolResult 与 ToolUse 同 turn
            //    严格按 tool_use_id 配对——顺序错了 API 会 400。
            let mut tool_results: Vec<ContentBlock> = Vec::with_capacity(tool_use_blocks.len());
            for ((id, _name, _input), output_opt) in
                tool_use_blocks.iter().zip(tool_outputs.into_iter())
            {
                let output = output_opt
                    .expect("dispatch_tool 必须为每个 tool_use_block 填回一个 ToolOutput");
                tool_results.push(ContentBlock::ToolResult {
                    tool_use_id: id.clone(),
                    content: output.content,
                    is_error: output.is_error,
                });
            }

            // 处理 directive notes
            let queued_notes = self.poll_queued_notes(agent_id);
            if !queued_notes.is_empty() {
                let notes_text = Self::format_notes_for_injection(&queued_notes);
                let note_ids: Vec<String> = queued_notes.iter().map(|(id, _)| id.clone()).collect();
                self.mark_notes_applied(&note_ids);
                // 走 emit_event_with_meta 可以把 note 实际内容也带过去，让前端把 directive
                // 高亮渲染（之前 only ack 行为，content 太干）。
                let notes_meta = serde_json::json!({
                    "applied_count": queued_notes.len(),
                    "note_ids": note_ids,
                    "notes": queued_notes.iter().map(|(_, c)| c.clone()).collect::<Vec<_>>(),
                });
                self.emit_event_with_meta(
                    agent_id,
                    step,
                    "note_applied",
                    &format!("Applied {} note(s)", queued_notes.len()),
                    Some(notes_meta),
                );
                tool_results.push(ContentBlock::Text { text: notes_text });
            }

            messages.push(Message {
                role: MessageRole::User,
                content: tool_results,
                cache_control: None,
            });

            if self.cancel_token.is_cancelled() {
                return self.finish_cancelled(agent_id, step);
            }
        }
    }

    /// 派发工具：`publish_artifact` 由 artifacts 模块直接处理（需要 DB），其它走 ToolExecutor。
    /// `task_complete` 已经在主循环里被截断，这里不会进来。
    ///
    /// FM-14：在真正执行前先过 approval_gate.maybe_intercept_tool；命中策略且用户拒绝，
    /// 则用一个 is_error=true 的 ToolOutput 直接替代结果，让 LLM 自然走"换种方式"路径。
    async fn dispatch_tool(
        &self,
        agent_id: &str,
        name: &str,
        input: &serde_json::Value,
    ) -> crate::tools::ToolOutput {
        if name == "publish_artifact" {
            return self.execute_publish_artifact(agent_id, input).await;
        }
        // Single-Agent Uplift Phase 1.2: todo_write 需要 DB（持久化到 agent_todos）+ emit
        // todo_update 事件让前端 TodoListPanel 实时刷新。所以特例化在 dispatch 层处理，
        // 与 publish_artifact 同模式，不污染 ToolExecutor 的 sandbox 边界。
        if name == crate::tools::TODO_WRITE_TOOL {
            return self.execute_todo_write(agent_id, input).await;
        }
        // Single-Agent Uplift Phase 2.4: enter_plan_mode 是纯"声明"工具——
        // 没有副作用，只把 plan 文本作为结构化事件 echo 给前端。直接在 dispatch
        // 层短路即可，不需要 ToolExecutor 沙箱、不需要 DB。
        if name == crate::tools::ENTER_PLAN_MODE_TOOL {
            return self.execute_enter_plan_mode(agent_id, input);
        }

        // FM-14 tool gate（write_file 到 protected_paths / shell_exec 到 destructive_commands）。
        if let Some(coord) = self
            .app_handle
            .try_state::<std::sync::Arc<crate::agent::approval::ApprovalCoordinator>>()
        {
            let db = self.app_handle.state::<Database>();
            let mission_id_opt: Option<String> = db
                .with_conn(|conn| queries::get_mission_id_for_agent(conn, agent_id))
                .ok()
                .flatten();
            if let Some(mission_id) = mission_id_opt {
                use crate::agent::approval_gate::{maybe_intercept_tool, ToolGateOutcome};
                match maybe_intercept_tool(
                    &self.app_handle,
                    coord.inner(),
                    db.inner(),
                    &self.cancel_token,
                    &mission_id,
                    agent_id,
                    name,
                    input,
                )
                .await
                {
                    ToolGateOutcome::Allow => {}
                    ToolGateOutcome::Rejected(out) => return out,
                }
            }
        }

        // shell_exec 走带 stream 的入口，把 stdout/stderr emit 给前端 Workspace。
        // 其它工具透传到普通 execute，行为不变。
        self.tool_executor
            .execute_with_stream(name, input, &self.app_handle, agent_id)
            .await
    }

    /// Single-Agent Uplift Phase 1.2: 执行 todo_write 工具。
    ///
    /// 行为：
    ///   1. 解析 input.todos[]（{id, content, status}）；任何不合法格式都 is_error=true
    ///      让 LLM 重试（保持工具一致的"结构化错误"哲学）。
    ///   2. 调 queries::replace_agent_todos 全量替换 agent_todos 表。
    ///   3. emit `todo_update` 事件携带 todos 数组，前端 TodoListPanel 直接消费。
    ///   4. 工具返回值是简短文本（"Updated N todo(s)..."）让 LLM 不再啰嗦地把
    ///      整个清单复述一遍——前端看不到也无所谓，反正它只显示 panel。
    async fn execute_todo_write(
        &self,
        agent_id: &str,
        input: &serde_json::Value,
    ) -> crate::tools::ToolOutput {
        use crate::tools::ToolOutput;

        #[derive(serde::Deserialize)]
        struct TodoIn {
            id: String,
            content: String,
            status: String,
        }
        #[derive(serde::Deserialize)]
        struct TodoWriteInput {
            todos: Vec<TodoIn>,
        }

        let parsed: TodoWriteInput = match serde_json::from_value(input.clone()) {
            Ok(v) => v,
            Err(e) => {
                return ToolOutput {
                    content: serde_json::json!({
                        "error": "parameter_error",
                        "message": format!("todo_write input parse failed: {e}"),
                    })
                    .to_string(),
                    is_error: true,
                };
            }
        };

        // 校验 status 取值；invalid 直接拒绝并把允许的值告诉 LLM。
        const ALLOWED: &[&str] = &["pending", "in_progress", "completed", "cancelled"];
        for td in &parsed.todos {
            if !ALLOWED.contains(&td.status.as_str()) {
                return ToolOutput {
                    content: serde_json::json!({
                        "error": "parameter_error",
                        "message": format!(
                            "Invalid status `{}` for todo `{}`. Allowed: pending / in_progress / completed / cancelled.",
                            td.status, td.id
                        ),
                    })
                    .to_string(),
                    is_error: true,
                };
            }
        }
        // 校验"最多一个 in_progress"——不强制（语义建议，不写进契约），
        // 但触发时给 LLM 一个 hint 让它自己收敛。这条不阻塞写盘。
        let in_progress_count = parsed
            .todos
            .iter()
            .filter(|t| t.status == "in_progress")
            .count();

        let db = self.app_handle.state::<Database>();
        let agent_owned = agent_id.to_string();
        let inputs: Vec<(String, String, String)> = parsed
            .todos
            .iter()
            .map(|t| (t.id.clone(), t.content.clone(), t.status.clone()))
            .collect();

        let write_result = db.with_conn(move |conn| {
            let refs: Vec<crate::db::queries::TodoInput<'_>> = inputs
                .iter()
                .map(|(id, content, status)| crate::db::queries::TodoInput {
                    id: id.as_str(),
                    content: content.as_str(),
                    status: status.as_str(),
                })
                .collect();
            crate::db::queries::replace_agent_todos(conn, &agent_owned, &refs)
        });

        if let Err(e) = write_result {
            return ToolOutput {
                content: serde_json::json!({
                    "error": "db_error",
                    "message": format!("Failed to persist todos: {e}"),
                })
                .to_string(),
                is_error: true,
            };
        }

        // emit todo_update 事件。meta 是完整 todo 列表（按数组顺序），前端按它整体刷新。
        let todos_meta: Vec<serde_json::Value> = parsed
            .todos
            .iter()
            .map(|t| serde_json::json!({
                "id": t.id,
                "content": t.content,
                "status": t.status,
            }))
            .collect();
        self.emit_event_with_meta(
            agent_id,
            self.read_agent_step(agent_id),
            "todo_update",
            &format!("Updated {} todo(s)", parsed.todos.len()),
            Some(serde_json::json!({ "todos": todos_meta })),
        );

        let mut summary = format!(
            "todos updated: {} item(s); {} pending, {} in_progress, {} completed",
            parsed.todos.len(),
            parsed.todos.iter().filter(|t| t.status == "pending").count(),
            in_progress_count,
            parsed
                .todos
                .iter()
                .filter(|t| t.status == "completed")
                .count(),
        );
        if in_progress_count > 1 {
            summary.push_str(
                " (note: prefer at most one in_progress at a time so progress is unambiguous)",
            );
        }
        ToolOutput {
            content: summary,
            is_error: false,
        }
    }

    /// Single-Agent Uplift Phase 2.4: 执行 enter_plan_mode。
    ///
    /// 没有副作用：解析 input.plan 文本，emit 一条带结构化 meta 的 `system_hint`
    /// 让前端醒目展示（黄色边框 + 多行 plan），返回简短确认字符串给 LLM。
    /// 用 `system_hint` kind 是因为它已经接好了"突出展示"渲染（SystemHintLine），
    /// 且不需要单独建一个新 kind 进 migration。
    fn execute_enter_plan_mode(
        &self,
        agent_id: &str,
        input: &serde_json::Value,
    ) -> crate::tools::ToolOutput {
        use crate::tools::ToolOutput;
        let plan = match input.get("plan").and_then(|v| v.as_str()) {
            Some(p) if !p.trim().is_empty() => p.to_string(),
            _ => {
                return ToolOutput {
                    content: serde_json::json!({
                        "error": "parameter_error",
                        "message": "enter_plan_mode requires non-empty `plan` (markdown text).",
                    })
                    .to_string(),
                    is_error: true,
                };
            }
        };
        let step = self.read_agent_step(agent_id);
        // 用 system_hint kind 复用前端 SystemHintLine 渲染。meta 里带 plan_mode flag
        // 让未来如果想拆出独立面板时可以前端查找。
        self.emit_event_with_meta(
            agent_id,
            step,
            "system_hint",
            &format!("[plan-mode] {}", plan.lines().next().unwrap_or("")),
            Some(serde_json::json!({
                "kind": "plan_mode",
                "plan": plan,
            })),
        );
        ToolOutput {
            content: format!(
                "Plan recorded ({} lines). Proceed with implementation; use todo_write to track step progress.",
                plan.lines().count()
            ),
            is_error: false,
        }
    }

    /// Helper：从 DB 读 agent.current_step，给那些没有 step 上下文的代码路径用
    /// （比如 todo_write 不在主循环 step 里被调用——其实在的，但这层封装让加点更随意）。
    fn read_agent_step(&self, agent_id: &str) -> u32 {
        let db = self.app_handle.state::<Database>();
        db.with_conn(|conn| {
            conn.query_row(
                "SELECT current_step FROM agents WHERE id = ?1",
                rusqlite::params![agent_id],
                |row| row.get::<_, i64>(0),
            )
            .map(|v| v as u32)
            .or_else(|_| Ok(0u32))
        })
        .unwrap_or(0)
    }

    /// 执行 publish_artifact 工具：基于 agent_id 反查 task_id / mission_id，
    /// 调用 artifacts 模块的校验 + 持久化路径。
    async fn execute_publish_artifact(
        &self,
        agent_id: &str,
        input: &serde_json::Value,
    ) -> crate::tools::ToolOutput {
        use crate::agent::artifacts::{record_publish, PublishArtifactInput};
        use crate::tools::ToolOutput;
        let parsed: PublishArtifactInput = match serde_json::from_value(input.clone()) {
            Ok(v) => v,
            Err(e) => {
                return ToolOutput {
                    content: serde_json::json!({
                        "error": "parameter_error",
                        "message": format!("publish_artifact input parse failed: {e}"),
                    })
                    .to_string(),
                    is_error: true,
                };
            }
        };
        let db = self.app_handle.state::<Database>();
        let agent = agent_id.to_string();
        let workspace = self.workspace_root.clone();
        let result = db.with_conn(move |conn| {
            let task_id = queries::get_task_id_for_agent(conn, &agent)?
                .ok_or_else(|| anyhow::anyhow!("agent {agent} has no task binding"))?;
            let mission_id = queries::get_mission_id_for_agent(conn, &agent)?
                .ok_or_else(|| anyhow::anyhow!("agent {agent} has no mission binding"))?;
            let decls_json: String = conn
                .query_row(
                    "SELECT produces_artifacts FROM tasks WHERE id = ?1",
                    rusqlite::params![&task_id],
                    |row| row.get::<_, String>(0),
                )
                .unwrap_or_else(|_| "[]".to_string());
            let decls: Vec<crate::agent::artifacts::ArtifactDecl> =
                serde_json::from_str(&decls_json).unwrap_or_default();
            record_publish(
                conn,
                &workspace,
                &mission_id,
                &task_id,
                &parsed,
                Some(&decls),
            )
            .map_err(|e| anyhow::anyhow!(e.to_string()))
        });
        match result {
            Ok(artifact) => {
                let _ = self.app_handle.emit(
                    "artifact-published",
                    serde_json::json!({
                        "agentId": agent_id,
                        "artifactId": artifact.id,
                        "missionId": artifact.mission_id,
                        "taskId": artifact.producer_task_id,
                        "type": artifact.artifact_type,
                        "localName": artifact.local_name,
                        "filePaths": artifact.file_paths,
                    }),
                );
                ToolOutput {
                    content: format!(
                        "Published artifact `{}` ({}) with {} file(s).",
                        artifact.local_name,
                        artifact.artifact_type,
                        artifact.file_paths.len()
                    ),
                    is_error: false,
                }
            }
            Err(e) => ToolOutput {
                content: serde_json::json!({
                    "error": "artifact_error",
                    "message": e.to_string(),
                })
                .to_string(),
                is_error: true,
            },
        }
    }

    /// 执行 guardrails 并决定后续动作。
    ///
    /// `task_description` 与 `summary` 一并传给 `LlmJudge`，作为评判的素材。
    async fn evaluate_completion(
        &self,
        agent_id: &str,
        step: u32,
        summary: &str,
        opts: &AgentRunOptions,
    ) -> CompletionOutcome {
        let db = self.app_handle.state::<Database>();
        let (task_id_opt, mission_id_opt) = match db.with_conn(|conn| {
            let task_id = queries::get_task_id_for_agent(conn, agent_id)?;
            let mission_id = queries::get_mission_id_for_agent(conn, agent_id)?;
            Ok((task_id, mission_id))
        }) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!("evaluate_completion: cannot resolve agent->task: {e}");
                return CompletionOutcome::Completed;
            }
        };

        let task_id = match task_id_opt {
            Some(t) => t,
            None => {
                tracing::warn!("Agent {agent_id} has no task; treating task_complete as success");
                return CompletionOutcome::Completed;
            }
        };
        let mission_id = mission_id_opt.unwrap_or_default();

        // 取 LLM provider（LlmJudge 用）。失败时退化为 None（LlmJudge 走 warn+pass 路径）。
        let (llm_for_judge, model_for_judge): (
            Option<std::sync::Arc<dyn crate::llm::LlmProvider>>,
            Option<String>,
        ) = match crate::commands::build_provider(&self.app_handle) {
            Ok((p, m)) => (Some(p), Some(m)),
            Err(_) => (None, None),
        };

        // 取 task description（LlmJudge 上下文）
        let task_desc_for_judge: Option<String> = db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT description FROM tasks WHERE id = ?1",
                    rusqlite::params![&task_id],
                    |row| row.get::<_, String>(0),
                )
                .map(Some)
                .or_else(|_| Ok(None))
            })
            .unwrap_or(None);

        let ctx = GuardrailContext {
            task_id: &task_id,
            mission_id: &mission_id,
            repo_root: &self.workspace_root,
            expected_output: opts.expected_output.clone(),
            produces: opts.produces.clone(),
            task_description: task_desc_for_judge,
            completion_summary: Some(summary.to_string()),
            llm: llm_for_judge,
            default_model: model_for_judge,
        };

        let guardrails: Vec<Guardrail> = if opts.guardrails.is_empty() {
            // 即便 task 没显式声明 guardrail，仍跑一次"产出对账" 以避免 Agent 谎称完成
            if !opts.produces.is_empty() {
                vec![Guardrail::ArtifactsExist]
            } else {
                Vec::new()
            }
        } else {
            opts.guardrails.clone()
        };

        if guardrails.is_empty() {
            self.emit_event(
                agent_id,
                step,
                "guardrail_summary",
                "no guardrails configured; accepting task_complete",
            );
            return CompletionOutcome::Completed;
        }

        let result = guardrail::run_guardrails(&guardrails, &ctx, &db).await;
        let serialized = serde_json::to_string(&result.reports).unwrap_or_default();
        // Single-Agent Uplift Phase 0.2: 把 reports 直接作为 meta，前端可按 array 渲染
        // 每条 guardrail 的 pass/fail badge + 折叠 detail，不再让用户看一坨 JSON 串。
        let reports_meta = serde_json::to_value(&result.reports).ok();
        self.emit_event_with_meta(
            agent_id,
            step,
            if result.all_passed {
                "guardrail_pass"
            } else {
                "guardrail_fail"
            },
            &serialized,
            reports_meta,
        );
        let _ = summary; // 仅用于事件层；持久化在 caller 处
        if result.all_passed {
            CompletionOutcome::Completed
        } else {
            CompletionOutcome::Retry {
                feedback: result.format_failure_for_agent(),
            }
        }
    }

    fn finish_cancelled(&self, agent_id: &str, step: u32) -> Result<AgentStatus> {
        self.expire_agent_notes(agent_id);
        self.emit_event(agent_id, step, "status_change", "cancelled");
        self.update_agent_status(agent_id, "cancelled");
        self.mark_task_failed_with_reason(agent_id, "cancelled", "cancelled: user stop");
        Ok(AgentStatus::Cancelled)
    }

    /// 在 engine 层把失败原因写入 `tasks.last_error` + `agents.error_message`，让前端 DAG /
    /// TaskDetailPanel 能直接 hover 看为什么红了。`reason` 推荐带分类前缀
    /// （`timeout:` / `max_steps:` / `guardrail:` / `cancelled:` / `llm_error:`）。
    fn mark_task_failed_with_reason(&self, agent_id: &str, status: &str, reason: &str) {
        let db = self.app_handle.state::<Database>();
        let aid = agent_id.to_string();
        let st = status.to_string();
        let r = reason.to_string();
        let _ = db.with_conn(move |conn| {
            queries::fail_task_for_agent(conn, &aid, &st, &r)?;
            conn.execute(
                "UPDATE agents SET error_message = COALESCE(error_message, ?2), \
                 updated_at = datetime('now') WHERE id = ?1",
                rusqlite::params![&aid, &r],
            )?;
            Ok(())
        });
    }

    fn emit_event(&self, agent_id: &str, step: u32, kind: &str, content: &str) {
        self.emit_event_with_meta(agent_id, step, kind, content, None);
    }

    /// Single-Agent Uplift Phase 0.2: emit + persist 一个带结构化 meta 的事件。
    /// `meta` 不为 None 时同时进 `agent-event` 推送和 `agent_events.meta` 列。
    /// 前端按 kind 决定如何解析（不同 kind 的 schema 各不一样，记得保持兼容）。
    fn emit_event_with_meta(
        &self,
        agent_id: &str,
        step: u32,
        kind: &str,
        content: &str,
        meta: Option<serde_json::Value>,
    ) {
        let _ = self.app_handle.emit(
            "agent-event",
            AgentEventPayload {
                agent_id: agent_id.to_string(),
                step,
                kind: kind.to_string(),
                content: content.to_string(),
                meta: meta.clone(),
            },
        );

        self.persist_event(agent_id, step, kind, content, meta);
    }

    fn persist_event(
        &self,
        agent_id: &str,
        step: u32,
        kind: &str,
        content: &str,
        meta: Option<serde_json::Value>,
    ) {
        let db = self.app_handle.state::<Database>();
        let event_id = Uuid::new_v4().to_string();
        let meta_str = meta.as_ref().map(|v| v.to_string());

        if let Err(e) = db.with_conn(|conn| {
            crate::db::queries::insert_event_with_meta(
                conn,
                &event_id,
                agent_id,
                step as i64,
                kind,
                content,
                meta_str.as_deref(),
            )
        }) {
            tracing::warn!("Failed to persist agent event (kind={kind}): {e}");
        }
    }

    fn update_agent_status(&self, agent_id: &str, status: &str) {
        let db = self.app_handle.state::<Database>();
        if let Err(e) = db.with_conn(|conn| {
            conn.execute(
                "UPDATE agents SET status = ?1, updated_at = datetime('now') WHERE id = ?2",
                rusqlite::params![status, agent_id],
            )?;
            Ok(())
        }) {
            tracing::warn!("Failed to update agent status: {e}");
        }
    }

    fn persist_completion_summary(&self, agent_id: &str, summary: &str) {
        if summary.trim().is_empty() {
            return;
        }
        let db = self.app_handle.state::<Database>();
        let agent = agent_id.to_string();
        let summary_owned = summary.to_string();
        if let Err(e) = db.with_conn(move |conn| {
            if let Some(task_id) = queries::get_task_id_for_agent(conn, &agent)? {
                conn.execute(
                    "UPDATE tasks SET completion_summary = ?1 WHERE id = ?2",
                    rusqlite::params![summary_owned, task_id],
                )?;
            }
            Ok(())
        }) {
            tracing::warn!("Failed to persist completion summary: {e}");
        }
    }

    fn persist_cost_record(
        &self,
        agent_id: &str,
        model: &str,
        input_tokens: u64,
        output_tokens: u64,
        cost_usd: f64,
    ) {
        let db = self.app_handle.state::<Database>();
        let record_id = Uuid::new_v4().to_string();
        if let Err(e) = db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO cost_records (id, agent_id, model, input_tokens, output_tokens, cost_usd)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![record_id, agent_id, model, input_tokens as i64, output_tokens as i64, cost_usd],
            )?;
            Ok(())
        }) {
            tracing::warn!("Failed to persist cost record: {e}");
        }
    }

    fn accumulate_agent_cost(
        &self,
        agent_id: &str,
        input_tokens: u64,
        output_tokens: u64,
        cost_usd: f64,
    ) {
        let db = self.app_handle.state::<Database>();
        let total_tokens = input_tokens + output_tokens;
        if let Err(e) = db.with_conn(|conn| {
            conn.execute(
                "UPDATE agents SET tokens_used = tokens_used + ?1, cost_usd = cost_usd + ?2, updated_at = datetime('now') WHERE id = ?3",
                rusqlite::params![total_tokens as i64, cost_usd, agent_id],
            )?;
            Ok(())
        }) {
            tracing::warn!("Failed to accumulate agent cost: {e}");
        }
    }

    fn update_agent_step(&self, agent_id: &str, step: u32) {
        let db = self.app_handle.state::<Database>();
        if let Err(e) = db.with_conn(|conn| {
            conn.execute(
                "UPDATE agents SET current_step = ?1, updated_at = datetime('now') WHERE id = ?2",
                rusqlite::params![step, agent_id],
            )?;
            Ok(())
        }) {
            tracing::warn!("Failed to update agent step: {e}");
        }
    }

    fn describe_llm_call(step: u32, messages: &[Message]) -> String {
        if messages.is_empty() {
            return format!("Step {step}: Analyzing task and planning approach");
        }
        let last_assistant = messages.iter().rev().find(|m| m.role == MessageRole::Assistant);
        if let Some(assistant_msg) = last_assistant {
            let tool_names: Vec<&str> = assistant_msg
                .content
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::ToolUse { name, .. } => Some(name.as_str()),
                    _ => None,
                })
                .collect();
            if !tool_names.is_empty() {
                let last_user = messages.last();
                let has_errors = last_user
                    .map(|m| {
                        m.content.iter().any(|b| matches!(b, ContentBlock::ToolResult { is_error: true, .. }))
                    })
                    .unwrap_or(false);
                let tools_str = tool_names.join(", ");
                return if has_errors {
                    format!("Step {step}: Reviewing results (with errors) from {tools_str}")
                } else {
                    format!("Step {step}: Reviewing results from {tools_str}")
                };
            }
        }
        format!("Step {step}: Continuing analysis")
    }

    // ---- FM-06: Note helpers ----

    fn poll_queued_notes(&self, agent_id: &str) -> Vec<(String, String)> {
        let db = self.app_handle.state::<Database>();
        db.with_conn(|conn| {
            let notes = queries::poll_queued_notes(conn, agent_id)?;
            Ok(notes.into_iter().map(|n| (n.id, n.content)).collect())
        })
        .unwrap_or_default()
    }

    fn mark_notes_applied(&self, note_ids: &[String]) {
        if note_ids.is_empty() {
            return;
        }
        let db = self.app_handle.state::<Database>();
        if let Err(e) = db.with_conn(|conn| queries::mark_notes_applied(conn, note_ids)) {
            tracing::warn!("Failed to mark notes as applied: {e}");
        }
    }

    fn expire_agent_notes(&self, agent_id: &str) {
        let db = self.app_handle.state::<Database>();
        match db.with_conn(|conn| queries::expire_notes_for_agent(conn, agent_id)) {
            Ok(count) if count > 0 => {
                tracing::info!("Expired {count} queued note(s) for agent {agent_id}");
            }
            Err(e) => {
                tracing::warn!("Failed to expire notes for agent {agent_id}: {e}");
            }
            _ => {}
        }
    }

    fn format_notes_for_injection(notes: &[(String, String)]) -> String {
        let mut out = String::from(
            "[System Note - Priority Update from Commander]:\n\
             The following directive(s) have been issued by the human commander. \
             You MUST follow them and adjust your work accordingly, \
             even if it means modifying files you have already written.\n\n",
        );
        for (i, (_, content)) in notes.iter().enumerate() {
            if notes.len() > 1 {
                out.push_str(&format!("{}. {content}\n\n", i + 1));
            } else {
                out.push_str(&format!("{content}\n\n"));
            }
        }
        out.push_str("Please take this into account in your next steps.");
        out
    }
}

enum CompletionOutcome {
    Completed,
    Retry { feedback: String },
}

fn render_guardrail_brief(guardrails: &[Guardrail]) -> String {
    if guardrails.is_empty() {
        return String::new();
    }
    let mut out = String::from("\n- Active guardrails for completion: ");
    let names: Vec<&str> = guardrails.iter().map(|g| g.name()).collect();
    out.push_str(&names.join(", "));
    out
}

fn render_produces_brief(produces: &[(String, String)]) -> String {
    if produces.is_empty() {
        return String::new();
    }
    let lines: Vec<String> = produces
        .iter()
        .map(|(name, ty)| format!("  - {name} ({ty})"))
        .collect();
    format!("\n\n## Required Artifacts\n{}", lines.join("\n"))
}

#[cfg(test)]
mod idle_retry_budget_tests {
    //! 回归测试：`next_idle_retry_budget` 是 idle-retry 设计的契约函数。
    //!
    //! 任何"简化"——例如永远 reset / 永远不 reset / reset 条件反了——都会让
    //! 用户经历两类回归：
    //!  - 永远 reset → 等价于无限 retry，遇到真挂的 provider task 会跑到 max_steps 才挂
    //!  - 永远不 reset → 回到任务级 budget 的老 bug，长 task 撞 3 次卡就 failed
    //!
    //! 守住这一组小不变量就能让未来的重构不至于摔进同一个坑。
    use super::next_idle_retry_budget;

    #[test]
    fn new_step_resets_to_default() {
        assert_eq!(next_idle_retry_budget(false, 0, 2), 2);
        assert_eq!(next_idle_retry_budget(false, 1, 2), 2);
        assert_eq!(next_idle_retry_budget(false, 2, 2), 2);
    }

    #[test]
    fn retry_continuation_keeps_current() {
        // 第一次 retry 后剩 1
        assert_eq!(next_idle_retry_budget(true, 1, 2), 1);
        // 第二次连续 retry 后剩 0
        assert_eq!(next_idle_retry_budget(true, 0, 2), 0);
    }

    #[test]
    fn full_step_lifecycle_two_retries_then_recover() {
        // 模拟一个 step：默认 budget=2，连续 2 次 retry，然后 step 成功 → 下一个 step 重置回 2
        let default = 2u32;

        // 进入新 step
        let mut budget = next_idle_retry_budget(false, 99, default);
        assert_eq!(budget, 2, "新 step 必须重置");

        // 第一次 IdleTimeout
        budget -= 1;
        budget = next_idle_retry_budget(true, budget, default);
        assert_eq!(budget, 1, "retry 续命，不重置");

        // 第二次 IdleTimeout
        budget -= 1;
        budget = next_idle_retry_budget(true, budget, default);
        assert_eq!(budget, 0, "再次 retry 续命，依然不重置");

        // 这一步 LLM 终于回了完整 response，进入下一个 step（resume = false）
        budget = next_idle_retry_budget(false, budget, default);
        assert_eq!(budget, 2, "step 成功后下一个 step 必须再次重置回满");
    }

    #[test]
    fn zero_default_disables_retry() {
        // 用户/未来配置如果把 budget 设为 0，整套机制等价于"卡就 fail"——
        // 这是合法配置，函数不应抛错或返回 surprising 值。
        assert_eq!(next_idle_retry_budget(false, 0, 0), 0);
        assert_eq!(next_idle_retry_budget(true, 0, 0), 0);
    }
}

#[cfg(test)]
mod context_compaction_tests {
    //! Single-Agent Uplift Phase 2.2 回归测试。
    //! 这两个不变量丢了用户立刻会感知到（要么 prompt 爆 ctx，要么 LLM 突然失忆）：
    //!   ① apply_tool_result_budget 幂等 + 只截 >8KB 的 ToolResult
    //!   ② microcompact 至少 8 messages 才动手；动手后总 token 一定下降
    use super::*;

    fn user_msg(text: &str) -> Message {
        Message {
            role: MessageRole::User,
            content: vec![ContentBlock::Text { text: text.to_string() }],
            cache_control: None,
        }
    }

    fn assistant_with_tool_use(name: &str, tool_use_id: &str) -> Message {
        Message {
            role: MessageRole::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: tool_use_id.to_string(),
                name: name.to_string(),
                input: serde_json::json!({}),
            }],
            cache_control: None,
        }
    }

    fn user_with_tool_result(tool_use_id: &str, content: &str) -> Message {
        Message {
            role: MessageRole::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: tool_use_id.to_string(),
                content: content.to_string(),
                is_error: false,
            }],
            cache_control: None,
        }
    }

    /// 短 ToolResult（< 8KB）不应被截断。
    #[test]
    fn budget_keeps_small_results_intact() {
        let mut messages = vec![user_with_tool_result("t1", "small content")];
        apply_tool_result_budget(&mut messages);
        if let ContentBlock::ToolResult { content, .. } = &messages[0].content[0] {
            assert_eq!(content, "small content");
        } else {
            panic!("expected ToolResult");
        }
    }

    /// 大 ToolResult 会被替换成 sentinel 字串，包含尾部内容。
    #[test]
    fn budget_truncates_large_results_with_tail() {
        let big = "X".repeat(20_000); // ~20K chars > 8K budget
        let mut messages = vec![user_with_tool_result("t1", &big)];
        apply_tool_result_budget(&mut messages);
        if let ContentBlock::ToolResult { content, .. } = &messages[0].content[0] {
            assert!(content.starts_with(TRUNCATED_SENTINEL_PREFIX));
            assert!(content.contains("Original size:"));
            assert!(content.len() < big.len() / 2, "截断后必须显著变短");
        } else {
            panic!("expected ToolResult");
        }
    }

    /// 截断幂等：连跑两次结果一致（不会把 sentinel 自己当原内容再截）。
    #[test]
    fn budget_is_idempotent() {
        let big = "X".repeat(20_000);
        let mut messages = vec![user_with_tool_result("t1", &big)];
        apply_tool_result_budget(&mut messages);
        let first = if let ContentBlock::ToolResult { content, .. } = &messages[0].content[0] {
            content.clone()
        } else {
            unreachable!()
        };
        apply_tool_result_budget(&mut messages);
        let second = if let ContentBlock::ToolResult { content, .. } = &messages[0].content[0] {
            content.clone()
        } else {
            unreachable!()
        };
        assert_eq!(first, second, "幂等：第二次应保持不变");
    }

    /// messages 太少时 microcompact 不动手——避免短任务上下文丢失。
    #[test]
    fn microcompact_noop_for_short_history() {
        let mut messages: Vec<Message> = (0..5).map(|i| user_msg(&format!("m{i}"))).collect();
        let report = microcompact(&mut messages);
        assert!(report.is_none(), "<8 messages 不该动手");
        assert_eq!(messages.len(), 5);
    }

    /// 长历史压缩：丢 1/3，最前面插 summary，token 总数下降。
    #[test]
    fn microcompact_drops_oldest_third_and_inserts_summary() {
        let mut messages: Vec<Message> = Vec::new();
        // 12 条 user/assistant 交替，每条带些"内容"凑出非零 token 估算
        for i in 0..6 {
            messages.push(user_msg(&format!("user msg {i} ").repeat(20)));
            messages.push(assistant_with_tool_use("read_file", &format!("tu_{i}")));
        }
        let before_count = messages.len();
        let before_tokens = approximate_tokens(&messages);

        let report = microcompact(&mut messages).expect("应当压缩");
        assert!(report.dropped_messages >= 1);
        // 摘要插到最前
        assert!(matches!(messages[0].role, MessageRole::User));
        if let ContentBlock::Text { text } = &messages[0].content[0] {
            assert!(text.starts_with("[context-compact]"));
            assert!(text.contains("read_file"), "summary 应汇总用过的工具名");
        } else {
            panic!("summary 必须是 Text block");
        }
        // 总数 = 原数 - drop + 1（summary）
        assert_eq!(messages.len(), before_count - report.dropped_messages + 1);
        // tokens 下降（用近似估算）
        let after_tokens = approximate_tokens(&messages);
        assert!(after_tokens < before_tokens, "压缩后 tokens 必须下降");
    }
}
