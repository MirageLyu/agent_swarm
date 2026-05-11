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
    /// Stream-idle 秒数：相邻两次 chunk 之间静默超过该值即视为卡死、提前终止本次调用。
    /// 0 表示不启用 idle 检测（仅依赖 reqwest 的全局 timeout）。
    stream_idle_secs: u64,
}

const DEFAULT_STREAM_IDLE_SECS: u64 = 60;

impl OpenAICompatProvider {
    pub fn new(api_key: String, base_url: String) -> Self {
        Self::with_stream_idle(api_key, base_url, DEFAULT_STREAM_IDLE_SECS)
    }

    pub fn with_stream_idle(api_key: String, base_url: String, stream_idle_secs: u64) -> Self {
        let base_url = base_url.trim_end_matches('/').to_string();
        // reqwest 的 .timeout 是"整个响应完成"的硬限。流式响应里我们靠 stream_idle_secs
        // 做"按 chunk 间隔"的更细粒度检测，所以把全局 timeout 放宽到 30 分钟，避免长 stream
        // 被强制截断；真正卡住时由 idle 检测精准杀掉。
        let client = Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(1800))
            .build()
            .unwrap_or_else(|_| Client::new());
        Self {
            client,
            api_key,
            base_url,
            stream_idle_secs,
        }
    }

    fn endpoint(&self) -> String {
        format!("{}/chat/completions", self.base_url)
    }

    pub(crate) fn convert_messages(&self, request: &LlmRequest) -> Vec<serde_json::Value> {
        let mut oai_messages = Vec::new();

        if let Some(system) = &request.system {
            // Split system prompt at __DYNAMIC_BOUNDARY__ for caching.
            // The static prefix gets cache_control, the dynamic suffix does not.
            if system.contains("__DYNAMIC_BOUNDARY__") {
                let parts: Vec<&str> = system.splitn(2, "═══ __DYNAMIC_BOUNDARY__ ═══").collect();
                if parts.len() == 2 {
                    let mut static_msg = json!({
                        "role": "system",
                        "content": parts[0].trim_end(),
                        "cache_control": {"type": "ephemeral"},
                    });
                    // Only include cache_control if the static prefix is substantial
                    if parts[0].len() < 500 {
                        static_msg.as_object_mut().unwrap().remove("cache_control");
                    }
                    oai_messages.push(static_msg);
                    oai_messages.push(json!({
                        "role": "system",
                        "content": parts[1].trim_start(),
                    }));
                } else {
                    oai_messages.push(json!({ "role": "system", "content": system }));
                }
            } else {
                oai_messages.push(json!({ "role": "system", "content": system }));
            }
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
                        let mut user_msg = json!({
                            "role": "user",
                            "content": text_parts.join("\n"),
                        });
                        if msg.cache_control.is_some() {
                            user_msg["cache_control"] = json!({"type": "ephemeral"});
                        }
                        oai_messages.push(user_msg);
                    }
                    oai_messages.extend(tool_results);
                }
                MessageRole::Assistant => {
                    let mut text = String::new();
                    let mut reasoning = String::new();
                    let mut tool_calls = Vec::new();

                    for block in &msg.content {
                        match block {
                            ContentBlock::Text { text: t } => text.push_str(t),
                            ContentBlock::Reasoning { text: r } => reasoning.push_str(r),
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
                    // DeepSeek-R1/V4、QwQ 等推理模型协议：上一轮的 reasoning_content
                    // 必须原样回传，否则下一轮 400。空 reasoning 不发送字段，
                    // 避免对不支持该字段的 provider 造成噪音。
                    if !reasoning.is_empty() {
                        msg_obj["reasoning_content"] = json!(reasoning);
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
            .enumerate()
            .map(|(_i, t)| {
                let mut tool = json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.input_schema,
                    }
                });
                // FM-10.4: Apply cache_control to the last tool definition
                if t.cache_control.is_some() {
                    tool["cache_control"] = json!({"type": "ephemeral"});
                }
                tool
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

        // Reasoning 必须放在 Text 之前，与 stream_chat 保持一致；
        // convert_messages 在下一轮把它合并回 reasoning_content 字段。
        if let Some(reasoning) = message["reasoning_content"].as_str() {
            if !reasoning.is_empty() {
                content.push(ContentBlock::Reasoning {
                    text: reasoning.to_string(),
                });
            }
        }

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
            cache_read_input_tokens: data["usage"]["cache_read_input_tokens"].as_u64().unwrap_or(0),
            cache_creation_input_tokens: data["usage"]["cache_creation_input_tokens"].as_u64().unwrap_or(0),
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

        let idle_dur = if self.stream_idle_secs == 0 {
            None
        } else {
            Some(std::time::Duration::from_secs(self.stream_idle_secs))
        };

        loop {
            // L1 stream-idle：如果配置了 idle 超时，每次 next 包一层 timeout；否则原始 next。
            let next_res = match idle_dur {
                Some(d) => match tokio::time::timeout(d, stream.next()).await {
                    Ok(v) => v,
                    Err(_) => {
                        let secs = self.stream_idle_secs;
                        if !full_text.is_empty() {
                            tracing::warn!(
                                "LLM stream idle for {secs}s after receiving {} chars; \
                                 returning partial response",
                                full_text.len()
                            );
                            break;
                        }
                        bail!(
                            "stream_idle_timeout: no chunk for {secs}s (consider increasing \
                             agent_step_idle_seconds in Settings, or the upstream LLM is hanging)"
                        );
                    }
                },
                None => stream.next().await,
            };
            let Some(chunk) = next_res else { break };
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
                        // FM-10.4: Parse cache metrics from DashScope response
                        if let Some(crt) = u["cache_read_input_tokens"].as_u64() {
                            usage.cache_read_input_tokens = crt;
                        }
                        if let Some(cct) = u["cache_creation_input_tokens"].as_u64() {
                            usage.cache_creation_input_tokens = cct;
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
        // Reasoning 在 Text 之前 push，确保下一轮 convert_messages
        // 看到的 assistant 块顺序是 reasoning → text → tool_use；
        // 这样 reasoning_content 字段能在序列化时正确集中拼接。
        if !full_reasoning.is_empty() {
            content.push(ContentBlock::Reasoning {
                text: full_reasoning,
            });
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn provider() -> OpenAICompatProvider {
        OpenAICompatProvider::new("k".into(), "https://example.com".into())
    }

    /// Bug: DeepSeek-R1 / V4 / QwQ 等推理模型上一轮的 reasoning_content
    /// 必须原样回传，否则 API 400。验证 convert_messages 把 Reasoning 块
    /// 合并进 assistant 消息的 reasoning_content 字段。
    #[test]
    fn convert_messages_emits_reasoning_content_for_assistant() {
        let req = LlmRequest {
            model: "deepseek-r1".into(),
            system: None,
            messages: vec![
                Message {
                    role: MessageRole::User,
                    content: vec![ContentBlock::Text { text: "hi".into() }],
                    cache_control: None,
                },
                Message {
                    role: MessageRole::Assistant,
                    content: vec![
                        ContentBlock::Reasoning { text: "let me think...".into() },
                        ContentBlock::Text { text: "hello".into() },
                    ],
                    cache_control: None,
                },
                Message {
                    role: MessageRole::User,
                    content: vec![ContentBlock::Text { text: "more".into() }],
                    cache_control: None,
                },
            ],
            tools: vec![],
            max_tokens: 100,
        };

        let oai = provider().convert_messages(&req);
        let assistant = oai
            .iter()
            .find(|m| m["role"] == "assistant")
            .expect("assistant message present");
        assert_eq!(assistant["reasoning_content"], "let me think...");
        assert_eq!(assistant["content"], "hello");
    }

    /// 没有 reasoning 时不要发 reasoning_content 字段（避免给不支持的模型噪音）。
    #[test]
    fn convert_messages_omits_reasoning_content_when_absent() {
        let req = LlmRequest {
            model: "gpt-4o".into(),
            system: None,
            messages: vec![Message {
                role: MessageRole::Assistant,
                content: vec![ContentBlock::Text { text: "hi".into() }],
                cache_control: None,
            }],
            tools: vec![],
            max_tokens: 100,
        };

        let oai = provider().convert_messages(&req);
        let assistant = &oai[0];
        assert!(assistant.get("reasoning_content").is_none());
    }

    /// 多个 Reasoning 块要拼接（streaming 切片场景）。
    #[test]
    fn convert_messages_concatenates_multiple_reasoning_blocks() {
        let req = LlmRequest {
            model: "qwq".into(),
            system: None,
            messages: vec![Message {
                role: MessageRole::Assistant,
                content: vec![
                    ContentBlock::Reasoning { text: "part1 ".into() },
                    ContentBlock::Reasoning { text: "part2".into() },
                    ContentBlock::Text { text: "answer".into() },
                ],
                cache_control: None,
            }],
            tools: vec![],
            max_tokens: 100,
        };

        let oai = provider().convert_messages(&req);
        assert_eq!(oai[0]["reasoning_content"], "part1 part2");
    }

    /// parse_response 要从 message.reasoning_content 解出 Reasoning 块。
    #[test]
    fn parse_response_extracts_reasoning_content() {
        let raw = json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "final answer",
                    "reasoning_content": "step 1, step 2",
                },
                "finish_reason": "stop",
            }],
            "usage": { "prompt_tokens": 10, "completion_tokens": 20 },
        });

        let resp = provider().parse_response(&raw).unwrap();
        let has_reasoning = resp
            .content
            .iter()
            .any(|b| matches!(b, ContentBlock::Reasoning { text } if text == "step 1, step 2"));
        let has_text = resp
            .content
            .iter()
            .any(|b| matches!(b, ContentBlock::Text { text } if text == "final answer"));
        assert!(has_reasoning, "Reasoning block missing from parsed response");
        assert!(has_text, "Text block missing from parsed response");
    }
}
