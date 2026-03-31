use anyhow::Result;
use std::sync::Arc;
use tauri::Emitter;
use tokio::sync::mpsc;

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
}

impl AgentEngine {
    pub fn new(
        provider: Arc<dyn LlmProvider>,
        workspace_root: std::path::PathBuf,
        app_handle: tauri::AppHandle,
    ) -> Self {
        Self {
            provider,
            tool_executor: ToolExecutor::new(workspace_root),
            app_handle,
        }
    }

    pub async fn run(
        &self,
        agent_id: &str,
        task_description: &str,
        max_steps: u32,
    ) -> Result<AgentStatus> {
        let tools = builtin_tools();
        let system = format!(
            "You are a coding agent. Complete the following task:\n\n{task_description}\n\n\
             Use the provided tools to read, write, and search files. \
             When the task is complete, respond with a summary of what you did."
        );

        let mut messages: Vec<Message> = Vec::new();
        let mut step: u32 = 0;

        self.emit_event(agent_id, step, "status", "executing");

        loop {
            if step >= max_steps {
                self.emit_event(agent_id, step, "error", "Max steps reached");
                return Ok(AgentStatus::Failed);
            }

            step += 1;
            self.emit_event(agent_id, step, "llm_call", &format!("Step {step}: calling LLM"));

            let request = LlmRequest {
                model: "claude-sonnet-4-20250514".to_string(),
                system: Some(system.clone()),
                messages: messages.clone(),
                tools: tools.clone(),
                max_tokens: 4096,
            };

            let (tx, mut rx) = mpsc::channel::<StreamChunk>(256);

            let provider = self.provider.clone();
            let req = request.clone();
            let response_handle = tokio::spawn(async move { provider.stream_chat(&req, tx).await });

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

            let has_tool_use = response
                .content
                .iter()
                .any(|b| matches!(b, ContentBlock::ToolUse { .. }));

            messages.push(Message {
                role: MessageRole::Assistant,
                content: response.content.clone(),
            });

            self.emit_event(
                agent_id,
                step,
                "checkpoint",
                &format!(
                    "tokens: {}in/{}out | stop: {}",
                    response.usage.input_tokens, response.usage.output_tokens, response.stop_reason
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
                return Ok(AgentStatus::Completed);
            }

            let mut tool_results: Vec<ContentBlock> = Vec::new();
            for block in &response.content {
                if let ContentBlock::ToolUse { id, name, input } = block {
                    self.emit_event(
                        agent_id,
                        step,
                        "tool_use",
                        &format!("{name}({})", serde_json::to_string(input).unwrap_or_default()),
                    );

                    let result = match self.tool_executor.execute(name, input).await {
                        Ok(output) => {
                            self.emit_event(agent_id, step, "tool_result", &output);
                            ContentBlock::ToolResult {
                                tool_use_id: id.clone(),
                                content: output,
                                is_error: false,
                            }
                        }
                        Err(e) => {
                            let err_msg = format!("Error: {e}");
                            self.emit_event(agent_id, step, "error", &err_msg);
                            ContentBlock::ToolResult {
                                tool_use_id: id.clone(),
                                content: err_msg,
                                is_error: true,
                            }
                        }
                    };
                    tool_results.push(result);
                }
            }

            messages.push(Message {
                role: MessageRole::User,
                content: tool_results,
            });
        }
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
    }
}
