use anyhow::Result;
use std::sync::Arc;
use tauri::{Emitter, Manager};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::db::{queries, Database};
use crate::llm::{
    ContentBlock, LlmProvider, LlmRequest, Message, MessageRole, StreamChunk, StreamChunkKind,
};
use crate::tools::{builtin_tools, ToolExecutor};

use super::types::*;

#[derive(Debug, Clone, serde::Serialize)]
struct AgentEventPayload {
    agent_id: String,
    step: u32,
    kind: String,
    content: String,
}

pub struct AgentEngine {
    provider: Arc<dyn LlmProvider>,
    tool_executor: ToolExecutor,
    app_handle: tauri::AppHandle,
    cancel_token: CancellationToken,
}

impl AgentEngine {
    pub fn new(
        provider: Arc<dyn LlmProvider>,
        workspace_root: std::path::PathBuf,
        app_handle: tauri::AppHandle,
        cancel_token: CancellationToken,
    ) -> Self {
        Self {
            provider,
            tool_executor: ToolExecutor::new(workspace_root),
            app_handle,
            cancel_token,
        }
    }

    pub async fn run(
        &self,
        agent_id: &str,
        task_description: &str,
        model: &str,
        max_steps: u32,
    ) -> Result<AgentStatus> {
        let tools = builtin_tools();
        let workspace_dir = self.tool_executor.workspace_display();
        let system = format!(
            "You are a coding agent working in the directory: {workspace_dir}\n\n\
             ## Task\n{task_description}\n\n\
             ## Instructions\n\
             - Use the provided tools to explore, read, write, and search files.\n\
             - All file paths are relative to the workspace root.\n\
             - Start by listing files with list_files to understand the workspace structure.\n\
             - ALWAYS provide all required parameters when calling a tool.\n\
             - When the task is complete, respond with a summary of what you did."
        );

        let mut messages: Vec<Message> = Vec::new();
        let mut step: u32 = 0;

        self.emit_event(agent_id, step, "status_change", "running");
        self.update_agent_status(agent_id, "running");

        loop {
            if self.cancel_token.is_cancelled() {
                return self.finish_cancelled(agent_id, step);
            }

            if step >= max_steps {
                self.emit_event(agent_id, step, "error", "Max steps reached");
                self.emit_event(agent_id, step, "status_change", "failed");
                self.update_agent_status(agent_id, "failed");
                self.expire_agent_notes(agent_id);
                return Ok(AgentStatus::Failed);
            }

            step += 1;
            self.update_agent_step(agent_id, step);

            let call_summary = Self::describe_llm_call(step, &messages);
            self.emit_event(agent_id, step, "llm_call", &call_summary);

            let request = LlmRequest {
                model: model.to_string(),
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

            let has_tool_use = response
                .content
                .iter()
                .any(|b| matches!(b, ContentBlock::ToolUse { .. }));

            messages.push(Message {
                role: MessageRole::Assistant,
                content: response.content.clone(),
            });

            let step_cost = self.provider.estimate_cost(
                model,
                response.usage.input_tokens,
                response.usage.output_tokens,
            );

            self.persist_cost_record(
                agent_id,
                model,
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
                    response.usage.input_tokens, response.usage.output_tokens, step_cost, response.stop_reason
                ),
            );

            if !has_tool_use {
                let summary = response
                    .content
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("");
                self.emit_event(agent_id, step, "message", &summary);
                self.emit_event(agent_id, step, "status_change", "completed");
                self.update_agent_status(agent_id, "completed");
                self.expire_agent_notes(agent_id);
                return Ok(AgentStatus::Completed);
            }

            let mut tool_results: Vec<ContentBlock> = Vec::new();
            for block in &response.content {
                if let ContentBlock::ToolUse { id, name, input } = block {
                    self.emit_event(
                        agent_id,
                        step,
                        "tool_use",
                        &format!(
                            "{name}({})",
                            serde_json::to_string(input).unwrap_or_default()
                        ),
                    );

                    let output = self.tool_executor.execute(name, input).await;

                    let event_kind = if output.is_error { "error" } else { "tool_result" };
                    self.emit_event(agent_id, step, event_kind, &output.content);

                    tool_results.push(ContentBlock::ToolResult {
                        tool_use_id: id.clone(),
                        content: output.content,
                        is_error: output.is_error,
                    });
                }
            }

            // Poll queued notes and inject as text in the conversation
            let queued_notes = self.poll_queued_notes(agent_id);
            if !queued_notes.is_empty() {
                let notes_text = Self::format_notes_for_injection(&queued_notes);
                let note_ids: Vec<String> =
                    queued_notes.iter().map(|(id, _)| id.clone()).collect();
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
            });

            if self.cancel_token.is_cancelled() {
                return self.finish_cancelled(agent_id, step);
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

        // Find the last assistant message to extract tool names
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
                // Check if any tool result was an error
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
