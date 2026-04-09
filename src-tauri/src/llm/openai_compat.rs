use anyhow::{bail, Result};
use async_trait::async_trait;
use reqwest::Client;
use serde_json::json;
use tokio::sync::mpsc;

use super::provider::LlmProvider;
use super::types::*;

pub struct OpenAICompatProvider {
    client: Client,
    api_key: String,
    base_url: String,
}

impl OpenAICompatProvider {
    pub fn new(api_key: String, base_url: String) -> Self {
        let base_url = base_url.trim_end_matches('/').to_string();
        let client = Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .unwrap_or_else(|_| Client::new());
        Self {
            client,
            api_key,
            base_url,
        }
    }

    fn endpoint(&self) -> String {
        format!("{}/chat/completions", self.base_url)
    }

    fn convert_messages(&self, request: &LlmRequest) -> Vec<serde_json::Value> {
        let mut oai_messages = Vec::new();

        if let Some(system) = &request.system {
            oai_messages.push(json!({
                "role": "system",
                "content": system,
            }));
        }

        for msg in &request.messages {
            match msg.role {
                MessageRole::User => {
                    let mut tool_results = Vec::new();
                    let mut text_parts = Vec::new();

                    for block in &msg.content {
                        match block {
                            ContentBlock::Text { text } => text_parts.push(text.clone()),
                            ContentBlock::ToolResult {
                                tool_use_id,
                                content,
                                ..
                            } => {
                                tool_results.push(json!({
                                    "role": "tool",
                                    "tool_call_id": tool_use_id,
                                    "content": content,
                                }));
                            }
                            _ => {}
                        }
                    }

                    if !text_parts.is_empty() {
                        oai_messages.push(json!({
                            "role": "user",
                            "content": text_parts.join("\n"),
                        }));
                    }
                    oai_messages.extend(tool_results);
                }
                MessageRole::Assistant => {
                    let mut text = String::new();
                    let mut tool_calls = Vec::new();

                    for block in &msg.content {
                        match block {
                            ContentBlock::Text { text: t } => text.push_str(t),
                            ContentBlock::ToolUse { id, name, input } => {
                                tool_calls.push(json!({
                                    "id": id,
                                    "type": "function",
                                    "function": {
                                        "name": name,
                                        "arguments": serde_json::to_string(input).unwrap_or_default(),
                                    }
                                }));
                            }
                            _ => {}
                        }
                    }

                    let mut msg_obj = json!({ "role": "assistant" });
                    if !text.is_empty() {
                        msg_obj["content"] = json!(text);
                    }
                    if !tool_calls.is_empty() {
                        msg_obj["tool_calls"] = json!(tool_calls);
                    }
                    oai_messages.push(msg_obj);
                }
            }
        }

        oai_messages
    }

    fn convert_tools(&self, tools: &[ToolDefinition]) -> Vec<serde_json::Value> {
        tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.input_schema,
                    }
                })
            })
            .collect()
    }

    fn build_body(&self, request: &LlmRequest, stream: bool) -> serde_json::Value {
        let mut body = json!({
            "model": request.model,
            "messages": self.convert_messages(request),
            "max_tokens": request.max_tokens,
            "stream": stream,
        });
        if stream {
            body["stream_options"] = json!({ "include_usage": true });
        }
        if !request.tools.is_empty() {
            body["tools"] = json!(self.convert_tools(&request.tools));
        }
        body
    }

    fn parse_response(&self, data: &serde_json::Value) -> Result<LlmResponse> {
        let choice = &data["choices"][0];
        let message = &choice["message"];
        let finish_reason = choice["finish_reason"]
            .as_str()
            .unwrap_or("stop")
            .to_string();

        let mut content = Vec::new();

        if let Some(text) = message["content"].as_str() {
            if !text.is_empty() {
                content.push(ContentBlock::Text {
                    text: text.to_string(),
                });
            }
        }

        if let Some(tool_calls) = message["tool_calls"].as_array() {
            for tc in tool_calls {
                let id = tc["id"].as_str().unwrap_or("").to_string();
                let name = tc["function"]["name"].as_str().unwrap_or("").to_string();
                let args_str = tc["function"]["arguments"].as_str().unwrap_or("{}");
                let input: serde_json::Value = match serde_json::from_str(args_str) {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(
                            "Failed to parse tool arguments for {name}: {e} | raw: {args_str}"
                        );
                        json!({})
                    }
                };
                content.push(ContentBlock::ToolUse { id, name, input });
            }
        }

        let stop_reason = match finish_reason.as_str() {
            "tool_calls" => "tool_use".to_string(),
            other => other.to_string(),
        };

        let usage = TokenUsage {
            input_tokens: data["usage"]["prompt_tokens"].as_u64().unwrap_or(0),
            output_tokens: data["usage"]["completion_tokens"].as_u64().unwrap_or(0),
        };

        Ok(LlmResponse {
            content,
            stop_reason,
            usage,
        })
    }
}

#[async_trait]
impl LlmProvider for OpenAICompatProvider {
    fn name(&self) -> &str {
        "openai_compat"
    }

    async fn chat(&self, request: &LlmRequest) -> Result<LlmResponse> {
        let body = self.build_body(request, false);

        tracing::debug!("OpenAI compat request to {}: {}", self.endpoint(), serde_json::to_string_pretty(&body).unwrap_or_default());

        let resp = self
            .client
            .post(&self.endpoint())
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                tracing::error!("OpenAI compat request failed: {e}");
                e
            })?;

        let status = resp.status();
        tracing::debug!("OpenAI compat response status: {status}");

        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            tracing::error!("OpenAI compat API error {status}: {text}");
            bail!("OpenAI compat API error {status}: {text}");
        }

        let data: serde_json::Value = resp.json().await?;
        tracing::debug!("OpenAI compat response parsed OK, model output length: {}", 
            data["choices"][0]["message"]["content"].as_str().map(|s| s.len()).unwrap_or(0));
        self.parse_response(&data)
    }

    async fn stream_chat(
        &self,
        request: &LlmRequest,
        tx: mpsc::Sender<StreamChunk>,
    ) -> Result<LlmResponse> {
        let body = self.build_body(request, true);

        let resp = self
            .client
            .post(&self.endpoint())
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!("OpenAI compat API error {status}: {text}");
        }

        let mut full_text = String::new();
        let mut full_reasoning = String::new();
        let mut tool_calls: Vec<(String, String, String)> = Vec::new(); // (id, name, args)
        let mut usage = TokenUsage::default();
        let mut finish_reason = String::from("stop");

        let mut stream = resp.bytes_stream();
        let mut buffer = String::new();
        use futures::StreamExt;

        while let Some(chunk) = stream.next().await {
            let chunk = match chunk {
                Ok(c) => c,
                Err(e) => {
                    if !full_text.is_empty() {
                        tracing::warn!("Stream decode error after receiving partial content ({} chars), using partial result: {e}", full_text.len());
                        break;
                    }
                    bail!("网络连接中断，请检查网络后重试 (stream error: {e})");
                }
            };
            buffer.push_str(&String::from_utf8_lossy(&chunk));

            while let Some(pos) = buffer.find('\n') {
                let line = buffer[..pos].trim().to_string();
                buffer = buffer[pos + 1..].to_string();

                if !line.starts_with("data: ") {
                    continue;
                }
                let json_str = &line[6..];
                if json_str == "[DONE]" {
                    continue;
                }

                if let Ok(data) = serde_json::from_str::<serde_json::Value>(json_str) {
                    if let Some(choice) = data["choices"].as_array().and_then(|c| c.first()) {
                        let delta = &choice["delta"];

                        // DashScope/Qwen reasoning models: reasoning_content
                        if let Some(reasoning) = delta["reasoning_content"].as_str() {
                            if !reasoning.is_empty() {
                                full_reasoning.push_str(reasoning);
                                let _ = tx
                                    .send(StreamChunk {
                                        kind: StreamChunkKind::ReasoningDelta,
                                        content: reasoning.to_string(),
                                    })
                                    .await;
                            }
                        }

                        if let Some(text) = delta["content"].as_str() {
                            if !text.is_empty() {
                                full_text.push_str(text);
                                let _ = tx
                                    .send(StreamChunk {
                                        kind: StreamChunkKind::TextDelta,
                                        content: text.to_string(),
                                    })
                                    .await;
                            }
                        }

                        if let Some(tcs) = delta["tool_calls"].as_array() {
                            for tc in tcs {
                                let idx = tc["index"].as_u64().unwrap_or(0) as usize;
                                while tool_calls.len() <= idx {
                                    tool_calls.push((String::new(), String::new(), String::new()));
                                }
                                if let Some(id) = tc["id"].as_str() {
                                    tool_calls[idx].0 = id.to_string();
                                }
                                if let Some(name) = tc["function"]["name"].as_str() {
                                    tool_calls[idx].1.push_str(name);
                                }
                                if let Some(args) = tc["function"]["arguments"].as_str() {
                                    tool_calls[idx].2.push_str(args);
                                }
                            }
                        }

                        if let Some(fr) = choice["finish_reason"].as_str() {
                            finish_reason = fr.to_string();
                        }
                    }

                    if let Some(u) = data.get("usage") {
                        if let Some(pt) = u["prompt_tokens"].as_u64() {
                            usage.input_tokens = pt;
                        }
                        if let Some(ct) = u["completion_tokens"].as_u64() {
                            usage.output_tokens = ct;
                        }
                    }
                }
            }
        }

        let _ = tx
            .send(StreamChunk {
                kind: StreamChunkKind::MessageStop,
                content: String::new(),
            })
            .await;

        let mut content: Vec<ContentBlock> = Vec::new();
        if !full_text.is_empty() {
            content.push(ContentBlock::Text { text: full_text });
        }
        for (id, name, args) in tool_calls {
            if !name.is_empty() {
                let input: serde_json::Value = match serde_json::from_str(&args) {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(
                            "Failed to parse streamed tool arguments for {name}: {e} | raw: {args}"
                        );
                        json!({})
                    }
                };
                content.push(ContentBlock::ToolUse { id, name, input });
            }
        }

        let stop_reason = match finish_reason.as_str() {
            "tool_calls" => "tool_use".to_string(),
            other => other.to_string(),
        };

        Ok(LlmResponse {
            content,
            stop_reason,
            usage,
        })
    }

    fn estimate_cost(&self, _model: &str, input_tokens: u64, output_tokens: u64) -> f64 {
        // DashScope/generic pricing — rough estimate
        (input_tokens as f64 * 2.0 + output_tokens as f64 * 6.0) / 1_000_000.0
    }
}
