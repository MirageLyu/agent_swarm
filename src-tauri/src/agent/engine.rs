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
    ContentBlock, LlmProvider, LlmRequest, Message, MessageRole, StreamChunk, StreamChunkKind,
};
use crate::tools::{coding_agent_tools_with_artifact_support, ToolExecutor, TASK_COMPLETE_TOOL};

use super::codebase_intel;
use super::guardrail::{self, Guardrail, GuardrailContext};
use super::types::*;

/// FR-11 默认值；Scheduler 可从配置覆盖。
pub const DEFAULT_AGENT_TIMEOUT_SECS: u64 = 600;
pub const DEFAULT_MAX_AGENT_STEPS: u32 = 50;
/// 当 LLM 连续 N 次不调用任何工具但又没调用 task_complete，就注入提示。
const MAX_CONSECUTIVE_NO_TOOL: u32 = 3;
/// 步数距上限只剩 N 时注入"剩余 N 步"提示。
const STEPS_REMAINING_HINT: u32 = 5;

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
                    "Agent {agent_id} hit timeout ({:?}); marking failed",
                    outer_dur
                );
                self.emit_event(
                    agent_id,
                    0,
                    "error",
                    &format!("Agent timed out after {}s", opts.timeout_secs),
                );
                self.emit_event(agent_id, 0, "status_change", "failed");
                self.update_agent_status(agent_id, "failed");
                self.expire_agent_notes(agent_id);
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
        let mut retries_left: u32 = opts.guardrail_retry_budget;
        let mut hinted_remaining_steps = false;

        self.emit_event(agent_id, step, "status_change", "running");
        self.update_agent_status(agent_id, "running");

        loop {
            if self.cancel_token.is_cancelled() {
                return self.finish_cancelled(agent_id, step);
            }

            if step >= opts.max_steps {
                self.emit_event(
                    agent_id,
                    step,
                    "error",
                    "max_steps_exceeded: agent did not call task_complete in time",
                );
                self.emit_event(agent_id, step, "status_change", "failed");
                self.update_agent_status(agent_id, "failed");
                self.expire_agent_notes(agent_id);
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
            let provider = self.provider.clone();
            let req = request.clone();
            let response_handle =
                tokio::spawn(async move { provider.stream_chat(&req, tx).await });

            let agent_id_owned = agent_id.to_string();
            let app_handle = self.app_handle.clone();
            let stream_step = step;
            tokio::spawn(async move {
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

            let response = response_handle.await??;

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
                            self.emit_event(
                                agent_id,
                                step,
                                "error",
                                "Guardrail retry budget exhausted",
                            );
                            self.emit_event(agent_id, step, "status_change", "failed");
                            self.update_agent_status(agent_id, "failed");
                            self.expire_agent_notes(agent_id);
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
                            let output = self.tool_executor.execute(name, input).await;
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
    async fn dispatch_tool(
        &self,
        agent_id: &str,
        name: &str,
        input: &serde_json::Value,
    ) -> crate::tools::ToolOutput {
        if name == "publish_artifact" {
            return self.execute_publish_artifact(agent_id, input).await;
        }
        self.tool_executor.execute(name, input).await
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
        Ok(AgentStatus::Cancelled)
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
