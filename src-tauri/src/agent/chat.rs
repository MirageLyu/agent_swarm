//! FM-15 v2.2 P4-S5: Follow-up Chat Agent (FR-15)。
//!
//! 设计要点：
//! - **不开 worktree**：直接在 mission.repo_path 的 main 分支上工作。
//! - **会话持久化**：所有用户/Agent 消息写入 `mission_chats` 表，刷新或切 view 都不丢。
//! - **小改动直接做**：≤ 3 文件 / ≤ 30 行 / 无新模块 → Agent 直接编辑并 `task_complete`。
//!   完成后由 `commit_main_workdir` 提交并做硬阈值校验，越界则视为失败并提示用户走 propose。
//! - **大改动走 propose**：Agent 调用 `propose_followup_mission` 工具；本模块发出
//!   `followup-proposed` 事件后立即返回，等待用户在前端确认。确认 → 走 `plan_mission` 创建
//!   子 mission；拒绝 → 前端可再次 `chat_send_message` 并设置 `force_direct=true` 强制直接执行。
//! - **流式输出**：通过 `chat-stream` 事件给前端，与 Coding Agent 的 `agent-stream` 不冲突。
//!
//! 依赖：复用 `tools::ToolExecutor` + `tools::chat_agent_tools()`。

use anyhow::{anyhow, Context, Result};
use std::path::PathBuf;
use std::sync::Arc;
use tauri::{Emitter, Manager};
use tokio::sync::mpsc;
use tokio::time::{timeout, Duration};
use uuid::Uuid;

use crate::db::{queries, Database};
use crate::git::WorktreeManager;
use crate::llm::{
    stream_chat_with_idle_guard, ContentBlock, LlmProvider, LlmRequest, Message, MessageRole,
    StreamChunk, StreamChunkKind, DEFAULT_STREAM_IDLE_TIMEOUT,
};
use crate::tools::{chat_agent_tools, ToolExecutor, PROPOSE_FOLLOWUP_TOOL, TASK_COMPLETE_TOOL};

/// FR-15.5 硬阈值。LLM 自由发挥 → 实际 commit diff 行数超过 30 即拒绝并要求走 propose。
pub const CHAT_LINES_HARD_LIMIT: usize = 30;
pub const CHAT_FILES_HARD_LIMIT: usize = 3;
pub const CHAT_DEFAULT_TIMEOUT_SECS: u64 = 300;
pub const CHAT_DEFAULT_MAX_STEPS: u32 = 20;

#[derive(Debug, Clone, serde::Serialize)]
pub struct ChatStreamPayload {
    pub mission_id: String,
    pub message_id: String,
    pub kind: String,
    pub content: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct FollowupProposedPayload {
    pub mission_id: String,
    pub chat_message_id: String,
    pub title: String,
    pub rationale: String,
    pub estimated_tasks: u32,
    pub request_summary: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ChatTurnSummary {
    pub mission_id: String,
    pub user_message_id: String,
    pub assistant_message_id: String,
    pub status: String,
    pub commit_hash: Option<String>,
    pub files_changed: Option<usize>,
    pub lines_changed: Option<usize>,
    pub error: Option<String>,
    pub proposed_followup: Option<FollowupProposedPayload>,
}

pub struct ChatAgent {
    provider: Arc<dyn LlmProvider>,
    model: String,
    repo_path: PathBuf,
    main_branch: String,
    app_handle: tauri::AppHandle,
}

impl ChatAgent {
    pub fn new(
        provider: Arc<dyn LlmProvider>,
        model: String,
        repo_path: PathBuf,
        main_branch: String,
        app_handle: tauri::AppHandle,
    ) -> Self {
        Self {
            provider,
            model,
            repo_path,
            main_branch,
            app_handle,
        }
    }

    /// 处理一条用户消息，运行一轮 chat-agent loop。
    ///
    /// `force_direct = true` 时即使大改动也强制直接做（用户拒绝了升级提议）。
    pub async fn handle_user_message(
        &self,
        mission_id: &str,
        user_content: &str,
        force_direct: bool,
    ) -> Result<ChatTurnSummary> {
        let db = self.app_handle.state::<Database>();

        // 1. 持久化用户消息
        let user_msg_id = Uuid::new_v4().to_string();
        db.with_conn(|c| {
            queries::insert_mission_chat(
                c,
                &user_msg_id,
                mission_id,
                "user",
                user_content,
                None,
                None,
                None,
            )
        })
        .context("insert user mission_chat")?;

        // 2. 生成 system prompt
        let system_prompt = self.build_system_prompt(mission_id, force_direct)?;

        // 3. 加载历史 chats 转成 LLM messages
        let history: Vec<queries::MissionChatRow> = db
            .with_conn(|c| queries::list_mission_chats(c, mission_id))
            .context("list mission chats")?;
        let messages = history_to_messages(&history);

        // 4. 跑 agent loop（带超时）
        let outer_dur = Duration::from_secs(CHAT_DEFAULT_TIMEOUT_SECS);
        let assistant_msg_id = Uuid::new_v4().to_string();

        let result = timeout(
            outer_dur,
            self.run_loop(
                mission_id,
                &assistant_msg_id,
                system_prompt,
                messages,
                force_direct,
            ),
        )
        .await;

        match result {
            Ok(Ok(turn)) => Ok(ChatTurnSummary {
                mission_id: mission_id.to_string(),
                user_message_id: user_msg_id,
                assistant_message_id: assistant_msg_id,
                ..turn
            }),
            Ok(Err(err)) => {
                let err_str = format!("{err:#}");
                let _ = db.with_conn(|c| {
                    queries::insert_mission_chat(
                        c,
                        &assistant_msg_id,
                        mission_id,
                        "system",
                        &format!("[error] {err_str}"),
                        None,
                        None,
                        None,
                    )
                });
                Ok(ChatTurnSummary {
                    mission_id: mission_id.to_string(),
                    user_message_id: user_msg_id,
                    assistant_message_id: assistant_msg_id,
                    status: "failed".into(),
                    commit_hash: None,
                    files_changed: None,
                    lines_changed: None,
                    error: Some(err_str),
                    proposed_followup: None,
                })
            }
            Err(_) => {
                let _ = db.with_conn(|c| {
                    queries::insert_mission_chat(
                        c,
                        &assistant_msg_id,
                        mission_id,
                        "system",
                        "[timeout] chat agent did not finish in time",
                        None,
                        None,
                        None,
                    )
                });
                Ok(ChatTurnSummary {
                    mission_id: mission_id.to_string(),
                    user_message_id: user_msg_id,
                    assistant_message_id: assistant_msg_id,
                    status: "timeout".into(),
                    commit_hash: None,
                    files_changed: None,
                    lines_changed: None,
                    error: Some(format!("timeout after {}s", CHAT_DEFAULT_TIMEOUT_SECS)),
                    proposed_followup: None,
                })
            }
        }
    }

    fn build_system_prompt(&self, mission_id: &str, force_direct: bool) -> Result<String> {
        let db = self.app_handle.state::<Database>();
        let mission = db
            .with_conn(|c| {
                c.query_row(
                    "SELECT title, description FROM missions WHERE id = ?1",
                    [mission_id],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
                )
                .map_err(anyhow::Error::from)
            })
            .context("load mission")?;

        let artifacts = db
            .with_conn(|c| queries::list_artifacts_for_mission(c, mission_id))
            .unwrap_or_default();

        let task_summaries = db
            .with_conn(|c| {
                let mut stmt = c.prepare(
                    "SELECT title, completion_summary FROM tasks
                     WHERE mission_id = ?1 AND status = 'completed'
                     ORDER BY created_at ASC",
                )?;
                let rows = stmt
                    .query_map([mission_id], |r| {
                        Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                anyhow::Ok(rows)
            })
            .unwrap_or_default();

        let mut artifacts_md = String::new();
        for a in &artifacts {
            artifacts_md.push_str(&format!(
                "- `{}` ({}): {}\n",
                a.local_name, a.artifact_type, a.summary
            ));
        }
        if artifacts_md.is_empty() {
            artifacts_md.push_str("(no artifacts published)\n");
        }

        let mut tasks_md = String::new();
        for (title, summary) in &task_summaries {
            let s = summary.as_deref().unwrap_or("(no summary)");
            tasks_md.push_str(&format!("- {title}: {s}\n"));
        }
        if tasks_md.is_empty() {
            tasks_md.push_str("(no completed tasks)\n");
        }

        let escalation_block = if force_direct {
            "\n\n## IMPORTANT — Escalation Override\n\
             The user has explicitly REJECTED any escalation. You MUST attempt the change \
             directly using the file tools. Do NOT call `propose_followup_mission` again.\n\
             If the change is genuinely too large, do as much as you safely can and \
             clearly state in `task_complete.summary` what was deferred."
                .to_string()
        } else {
            format!(
                "\n\n## Escalation Policy (FR-15.4)\n\
                 Estimate the scope BEFORE editing:\n\
                 - If your edit will touch ≤ {files_limit} files AND ≤ {lines_limit} lines AND \
                   creates no new module / dependency → make the edit directly with file tools, \
                   then call `task_complete`.\n\
                 - Otherwise call `propose_followup_mission` (the user will see a confirm dialog).\n\
                 Hard guardrail: if the actual commit exceeds these limits the system will \
                 reject your changes and the user will be asked to escalate.",
                files_limit = CHAT_FILES_HARD_LIMIT,
                lines_limit = CHAT_LINES_HARD_LIMIT,
            )
        };

        Ok(format!(
            "You are Miragenty's Follow-up Chat Agent for mission `{mission_id}`.\n\n\
             Working directory (already on the `{main}` branch — DO NOT switch branches): \
             {workdir}\n\n\
             ## Mission\n**{title}**\n\n{desc}\n\n\
             ## Completed Tasks\n{tasks_md}\n\
             ## Published Artifacts\n{artifacts_md}\n\
             ## Tools\n\
             - `read_file` / `write_file` / `edit_file` / `grep` / `glob` / `list_files` / `shell_exec` for code edits.\n\
             - `propose_followup_mission` to escalate large requests.\n\
             - `task_complete` to signal you're done; the system will then commit the working \
               directory to `{main}`.\n\
             {escalation_block}",
            mission_id = mission_id,
            main = self.main_branch,
            workdir = self.repo_path.display(),
            title = mission.0,
            desc = if mission.1.trim().is_empty() { "(no description)".to_string() } else { mission.1 },
        ))
    }

    async fn run_loop(
        &self,
        mission_id: &str,
        assistant_msg_id: &str,
        system_prompt: String,
        prior: Vec<Message>,
        force_direct: bool,
    ) -> Result<ChatTurnSummary> {
        let tools = chat_agent_tools();
        let executor = ToolExecutor::new(self.repo_path.clone());
        let mut messages = prior;
        let mut consecutive_no_tool: u32 = 0;
        let db = self.app_handle.state::<Database>();

        for step in 0..CHAT_DEFAULT_MAX_STEPS {
            let request = LlmRequest {
                model: self.model.clone(),
                system: Some(system_prompt.clone()),
                messages: messages.clone(),
                tools: tools.clone(),
                max_tokens: 4096,
                provider_extras: None,
            };

            let (tx, mut rx) = mpsc::channel::<StreamChunk>(256);
            let app_handle = self.app_handle.clone();
            let mission_id_owned = mission_id.to_string();
            let msg_id_owned = assistant_msg_id.to_string();
            let stream_step = step;
            let forwarder = tokio::spawn(async move {
                while let Some(chunk) = rx.recv().await {
                    if let StreamChunkKind::TextDelta = chunk.kind {
                        let _ = app_handle.emit(
                            "chat-stream",
                            ChatStreamPayload {
                                mission_id: mission_id_owned.clone(),
                                message_id: msg_id_owned.clone(),
                                kind: format!("text_delta:{stream_step}"),
                                content: chunk.content,
                            },
                        );
                    }
                }
            });

            // Idle 看门狗复用 llm::stream_guard，避免聊天卡死无人察觉。
            let response = stream_chat_with_idle_guard(
                self.provider.clone(),
                request,
                tx,
                DEFAULT_STREAM_IDLE_TIMEOUT,
            )
            .await
            .map_err(|e| anyhow::anyhow!(e.user_message_zh()))?;
            let _ = forwarder.await;

            messages.push(Message {
                role: MessageRole::Assistant,
                content: response.content.clone(),
                cache_control: None,
            });

            // 收集 tool_use 和文本
            let mut tool_uses: Vec<(String, String, serde_json::Value)> = Vec::new();
            let mut text_parts: Vec<String> = Vec::new();
            for block in &response.content {
                match block {
                    ContentBlock::ToolUse { id, name, input } => {
                        tool_uses.push((id.clone(), name.clone(), input.clone()));
                    }
                    ContentBlock::Text { text } => text_parts.push(text.clone()),
                    _ => {}
                }
            }

            // 处理 propose_followup_mission（仅非 force_direct 才允许）
            if let Some((_, _, input)) = tool_uses
                .iter()
                .find(|(_, n, _)| n == PROPOSE_FOLLOWUP_TOOL)
                .cloned()
            {
                if force_direct {
                    let hint = "[System] Escalation rejected by user — call task_complete \
                                or continue editing instead of propose_followup_mission.";
                    messages.push(Message {
                        role: MessageRole::User,
                        content: vec![ContentBlock::Text { text: hint.into() }],
                        cache_control: None,
                    });
                    continue;
                }

                let title = input
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let rationale = input
                    .get("rationale")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let estimated_tasks = input
                    .get("estimated_tasks")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as u32;
                let request_summary = input
                    .get("request_summary")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                let payload = FollowupProposedPayload {
                    mission_id: mission_id.to_string(),
                    chat_message_id: assistant_msg_id.to_string(),
                    title: title.clone(),
                    rationale: rationale.clone(),
                    estimated_tasks,
                    request_summary: request_summary.clone(),
                };

                // 持久化 assistant 消息（含 tool_calls JSON）
                let tool_calls_json = serde_json::to_string(&payload).ok();
                let combined_text = format!(
                    "{}\n\n[propose_followup_mission] {} — {}",
                    text_parts.join("\n"),
                    title,
                    rationale
                );
                db.with_conn(|c| {
                    queries::insert_mission_chat(
                        c,
                        assistant_msg_id,
                        mission_id,
                        "assistant",
                        &combined_text,
                        tool_calls_json.as_deref(),
                        None,
                        None,
                    )
                })
                .ok();

                let _ = self.app_handle.emit("followup-proposed", payload.clone());

                // FM-14: 同步写入统一审批队列；这是异步审批（chat 不阻塞），
                // 用户既可以在 ChatPanel 内点确认，也可以在 ApprovalQueue 里处理。
                // 任何路径上的 approve/reject 都会被 commands/chat.rs 里的 confirm_/reject_
                // followup_proposal 解析（见 Slice 3 改造）。
                let approval_id = uuid::Uuid::new_v4().to_string();
                let payload_json = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".into());
                let title_for_q = format!("Follow-up mission: {}", title);
                let reason_for_q = if rationale.is_empty() {
                    format!("Estimated {estimated_tasks} task(s); chat agent suggests escalating.")
                } else {
                    rationale.clone()
                };
                let timeout_secs = self
                    .app_handle
                    .try_state::<crate::commands::ConfigManager>()
                    .map(|c| c.get_config_snapshot().approval_policy.timeout_seconds as i64)
                    .unwrap_or(crate::agent::approval::DEFAULT_APPROVAL_TIMEOUT_SECS);
                let context_summary_text = trim_context(&combined_text, 240);
                let new_req = crate::db::queries::NewApproval {
                    id: &approval_id,
                    mission_id,
                    kind: "escalation",
                    agent_id: None,
                    planner_session_id: None,
                    chat_message_id: Some(assistant_msg_id),
                    title: &title_for_q,
                    payload: &payload_json,
                    reason: &reason_for_q,
                    context_summary: context_summary_text.as_str(),
                    timeout_seconds: timeout_secs,
                };
                if let Err(e) = db.with_conn(|c| crate::db::queries::insert_approval(c, &new_req)) {
                    tracing::warn!(
                        "[chat] failed to mirror followup proposal to approval_requests: {e}"
                    );
                } else {
                    let _ = self.app_handle.emit(
                        "approval-requested",
                        serde_json::json!({
                            "request_id": approval_id,
                            "mission_id": mission_id,
                            "kind": "escalation",
                            "title": title_for_q,
                        }),
                    );
                }

                return Ok(ChatTurnSummary {
                    mission_id: mission_id.to_string(),
                    user_message_id: String::new(),
                    assistant_message_id: assistant_msg_id.to_string(),
                    status: "proposed".into(),
                    commit_hash: None,
                    files_changed: None,
                    lines_changed: None,
                    error: None,
                    proposed_followup: Some(payload),
                });
            }

            // 处理 task_complete → 提交 main 工作区
            if let Some((_, _, input)) = tool_uses
                .iter()
                .find(|(_, n, _)| n == TASK_COMPLETE_TOOL)
                .cloned()
            {
                let summary = input
                    .get("summary")
                    .and_then(|v| v.as_str())
                    .unwrap_or("(no summary)")
                    .to_string();

                let manager = WorktreeManager::with_main_branch(
                    self.repo_path.clone(),
                    self.main_branch.clone(),
                );
                let commit_msg = format!("chat: {}", truncate(&summary, 70));
                let outcome_res =
                    tokio::task::spawn_blocking(move || manager.commit_main_workdir(&commit_msg))
                        .await
                        .map_err(|e| anyhow!("commit task panicked: {e}"))?;

                let combined_text =
                    format!("{}\n\n[task_complete] {}", text_parts.join("\n"), summary);

                match outcome_res {
                    Ok(Some(outcome)) => {
                        // FR-15.5 硬阈值校验
                        if outcome.lines_changed > CHAT_LINES_HARD_LIMIT
                            || outcome.files_changed > CHAT_FILES_HARD_LIMIT
                        {
                            let warn = format!(
                                "[guardrail] commit exceeded chat hard limit: \
                                 {} files, {} lines (limit {} files / {} lines). \
                                 Please re-run with `propose_followup_mission`.",
                                outcome.files_changed,
                                outcome.lines_changed,
                                CHAT_FILES_HARD_LIMIT,
                                CHAT_LINES_HARD_LIMIT
                            );
                            db.with_conn(|c| {
                                queries::insert_mission_chat(
                                    c,
                                    assistant_msg_id,
                                    mission_id,
                                    "assistant",
                                    &combined_text,
                                    None,
                                    None,
                                    None,
                                )?;
                                let warn_id = Uuid::new_v4().to_string();
                                queries::insert_mission_chat(
                                    c, &warn_id, mission_id, "system", &warn, None, None, None,
                                )
                            })
                            .ok();
                            return Ok(ChatTurnSummary {
                                mission_id: mission_id.to_string(),
                                user_message_id: String::new(),
                                assistant_message_id: assistant_msg_id.to_string(),
                                status: "rejected_oversize".into(),
                                commit_hash: Some(outcome.commit_hash),
                                files_changed: Some(outcome.files_changed),
                                lines_changed: Some(outcome.lines_changed),
                                error: Some(warn),
                                proposed_followup: None,
                            });
                        }

                        let artifact_refs = serde_json::to_string(&outcome.changed_paths).ok();
                        db.with_conn(|c| {
                            queries::insert_mission_chat(
                                c,
                                assistant_msg_id,
                                mission_id,
                                "assistant",
                                &combined_text,
                                None,
                                artifact_refs.as_deref(),
                                None,
                            )
                        })
                        .ok();
                        return Ok(ChatTurnSummary {
                            mission_id: mission_id.to_string(),
                            user_message_id: String::new(),
                            assistant_message_id: assistant_msg_id.to_string(),
                            status: "committed".into(),
                            commit_hash: Some(outcome.commit_hash),
                            files_changed: Some(outcome.files_changed),
                            lines_changed: Some(outcome.lines_changed),
                            error: None,
                            proposed_followup: None,
                        });
                    }
                    Ok(None) => {
                        // 没有改动也算成功完成（纯回答）
                        db.with_conn(|c| {
                            queries::insert_mission_chat(
                                c,
                                assistant_msg_id,
                                mission_id,
                                "assistant",
                                &combined_text,
                                None,
                                None,
                                None,
                            )
                        })
                        .ok();
                        return Ok(ChatTurnSummary {
                            mission_id: mission_id.to_string(),
                            user_message_id: String::new(),
                            assistant_message_id: assistant_msg_id.to_string(),
                            status: "answered".into(),
                            commit_hash: None,
                            files_changed: Some(0),
                            lines_changed: Some(0),
                            error: None,
                            proposed_followup: None,
                        });
                    }
                    Err(e) => {
                        let err_str = format!("{e:#}");
                        db.with_conn(|c| {
                            queries::insert_mission_chat(
                                c,
                                assistant_msg_id,
                                mission_id,
                                "system",
                                &format!("[commit-failed] {err_str}"),
                                None,
                                None,
                                None,
                            )
                        })
                        .ok();
                        return Ok(ChatTurnSummary {
                            mission_id: mission_id.to_string(),
                            user_message_id: String::new(),
                            assistant_message_id: assistant_msg_id.to_string(),
                            status: "commit_failed".into(),
                            commit_hash: None,
                            files_changed: None,
                            lines_changed: None,
                            error: Some(err_str),
                            proposed_followup: None,
                        });
                    }
                }
            }

            // 普通文件工具：执行后追加 tool_result 让 LLM 继续
            if !tool_uses.is_empty() {
                consecutive_no_tool = 0;
                let mut tool_results: Vec<ContentBlock> = Vec::new();
                for (id, name, input) in &tool_uses {
                    if name == TASK_COMPLETE_TOOL || name == PROPOSE_FOLLOWUP_TOOL {
                        continue;
                    }
                    let output = executor.execute(name, input).await;
                    tool_results.push(ContentBlock::ToolResult {
                        tool_use_id: id.clone(),
                        content: output.content,
                        is_error: output.is_error,
                    });
                }
                if !tool_results.is_empty() {
                    messages.push(Message {
                        role: MessageRole::User,
                        content: tool_results,
                        cache_control: None,
                    });
                }
                continue;
            }

            // 没有 tool_use 也没有 task_complete：注入提示，最多容忍 2 次
            consecutive_no_tool += 1;
            if consecutive_no_tool >= 2 {
                let combined = text_parts.join("\n");
                db.with_conn(|c| {
                    queries::insert_mission_chat(
                        c,
                        assistant_msg_id,
                        mission_id,
                        "assistant",
                        &combined,
                        None,
                        None,
                        None,
                    )
                })
                .ok();
                return Ok(ChatTurnSummary {
                    mission_id: mission_id.to_string(),
                    user_message_id: String::new(),
                    assistant_message_id: assistant_msg_id.to_string(),
                    status: "answered".into(),
                    commit_hash: None,
                    files_changed: Some(0),
                    lines_changed: Some(0),
                    error: None,
                    proposed_followup: None,
                });
            }
            messages.push(Message {
                role: MessageRole::User,
                content: vec![ContentBlock::Text {
                    text: "[System] If the user only wanted an explanation, call \
                          task_complete with a one-line summary. Otherwise use the file \
                          tools and then task_complete."
                        .into(),
                }],
                cache_control: None,
            });
        }

        Err(anyhow!("chat agent exceeded max steps"))
    }
}

fn history_to_messages(history: &[queries::MissionChatRow]) -> Vec<Message> {
    let mut out = Vec::new();
    for row in history {
        let role = match row.role.as_str() {
            "assistant" => MessageRole::Assistant,
            // "system" 或其它 → 当作 user notice 注入，避免 OpenAI/Anthropic role 问题
            _ => MessageRole::User,
        };
        out.push(Message {
            role,
            content: vec![ContentBlock::Text {
                text: row.content.clone(),
            }],
            cache_control: None,
        });
    }
    out
}

fn truncate(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect::<String>() + "…"
}

/// FM-14: 简易上下文摘要（按字符截断）。供 approval_requests.context_summary 使用。
fn trim_context(s: &str, max: usize) -> String {
    truncate(s, max)
}
