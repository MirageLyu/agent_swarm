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
    StreamChunk, StreamChunkKind, StreamGuardError, DEFAULT_STREAM_IDLE_TIMEOUT,
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

#[derive(Debug, Clone, serde::Serialize)]
struct AgentEventPayload {
    agent_id: String,
    step: u32,
    kind: String,
    content: String,
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
            let forwarder = tokio::spawn(async move {
                while let Some(chunk) = rx.recv().await {
                    if let StreamChunkKind::TextDelta = chunk.kind {
                        let _ = app_handle.emit(
                            "agent-stream",
                            AgentEventPayload {
                                agent_id: agent_id_owned.clone(),
                                step: stream_step,
                                kind: "text_delta".to_string(),
                                content: chunk.content,
                            },
                        );
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

                self.emit_event(
                    agent_id,
                    step,
                    "tool_use",
                    &format!("task_complete({{\"summary\": ...}})"),
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

            let mut tool_results: Vec<ContentBlock> = Vec::new();
            for (id, name, input) in &tool_use_blocks {
                self.emit_event(
                    agent_id,
                    step,
                    "tool_use",
                    &format!(
                        "{name}({})",
                        serde_json::to_string(input).unwrap_or_default()
                    ),
                );
                let output = self
                    .dispatch_tool(agent_id, name, input)
                    .await;
                let event_kind = if output.is_error { "error" } else { "tool_result" };
                self.emit_event(agent_id, step, event_kind, &output.content);
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
                let _ = self.app_handle.emit(
                    "agent-event",
                    AgentEventPayload {
                        agent_id: agent_id.to_string(),
                        step,
                        kind: "note_applied".to_string(),
                        content: format!("Applied {} note(s)", queued_notes.len()),
                    },
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
        self.emit_event(
            agent_id,
            step,
            if result.all_passed {
                "guardrail_pass"
            } else {
                "guardrail_fail"
            },
            &serialized,
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
        let _ = self.app_handle.emit(
            "agent-event",
            AgentEventPayload {
                agent_id: agent_id.to_string(),
                step,
                kind: kind.to_string(),
                content: content.to_string(),
            },
        );

        self.persist_event(agent_id, step, kind, content);
    }

    fn persist_event(&self, agent_id: &str, step: u32, kind: &str, content: &str) {
        let db = self.app_handle.state::<Database>();
        let event_id = Uuid::new_v4().to_string();

        if let Err(e) = db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO agent_events (id, agent_id, step, kind, content) VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![event_id, agent_id, step as i64, kind, content],
            )?;
            Ok(())
        }) {
            tracing::warn!("Failed to persist agent event: {e}");
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
