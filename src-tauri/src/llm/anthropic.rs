use anyhow::{bail, Result};
use async_trait::async_trait;
use reqwest::Client;
use serde_json::json;
use tokio::sync::mpsc;

use super::provider::LlmProvider;
use super::types::*;

pub struct AnthropicProvider {
    client: Client,
    api_key: String,
    /// 见 `OpenAICompatProvider::stream_idle_secs`。
    stream_idle_secs: u64,
}

const DEFAULT_STREAM_IDLE_SECS: u64 = 60;

impl AnthropicProvider {
    pub fn new(api_key: String) -> Self {
        Self::with_stream_idle(api_key, DEFAULT_STREAM_IDLE_SECS)
    }

    pub fn with_stream_idle(api_key: String, stream_idle_secs: u64) -> Self {
        let client = Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(1800))
            .build()
            .unwrap_or_else(|_| Client::new());
        Self {
            client,
            api_key,
            stream_idle_secs,
        }
    }

    fn build_body(&self, request: &LlmRequest, stream: bool) -> serde_json::Value {
        // Reasoning 块来自 OpenAI-compat 推理模型协议，schema 与 Anthropic
        // 自身的 thinking 块不同，发到 Anthropic 会报"unknown content type"。
        // 这里逐条 message 过滤掉 Reasoning，其它 variant 原样保留。
        let messages: Vec<Message> = request
            .messages
            .iter()
            .map(|m| Message {
                role: m.role.clone(),
                content: m
                    .content
                    .iter()
                    .filter(|b| !matches!(b, ContentBlock::Reasoning { .. }))
                    .cloned()
                    .collect(),
                cache_control: m.cache_control.clone(),
            })
            .collect();

        let mut body = json!({
            "model": request.model,
            "max_tokens": request.max_tokens,
            "messages": messages,
            "stream": stream,
        });
        if let Some(system) = &request.system {
            body["system"] = json!(system);
        }
        if !request.tools.is_empty() {
            body["tools"] = json!(request.tools);
        }
        body
    }
}

#[async_trait]
impl LlmProvider for AnthropicProvider {
    fn name(&self) -> &str {
        "anthropic"
    }

    async fn chat(&self, request: &LlmRequest) -> Result<LlmResponse> {
        let body = self.build_body(request, false);
        let resp = self
            .client
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!("Anthropic API error {status}: {text}");
        }

        let data: serde_json::Value = resp.json().await?;
        let content: Vec<ContentBlock> = serde_json::from_value(data["content"].clone())?;
        let stop_reason = data["stop_reason"]
            .as_str()
            .unwrap_or("end_turn")
            .to_string();
        let usage = TokenUsage {
            input_tokens: data["usage"]["input_tokens"].as_u64().unwrap_or(0),
            output_tokens: data["usage"]["output_tokens"].as_u64().unwrap_or(0),
            cache_read_input_tokens: data["usage"]["cache_read_input_tokens"]
                .as_u64()
                .unwrap_or(0),
            cache_creation_input_tokens: data["usage"]["cache_creation_input_tokens"]
                .as_u64()
                .unwrap_or(0),
        };

        Ok(LlmResponse {
            content,
            stop_reason,
            usage,
        })
    }

    async fn stream_chat(
        &self,
        request: &LlmRequest,
        tx: mpsc::Sender<StreamChunk>,
    ) -> Result<LlmResponse> {
        let body = self.build_body(request, true);
        let resp = self
            .client
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!("Anthropic API error {status}: {text}");
        }

        let mut full_text = String::new();
        let mut usage = TokenUsage::default();
        let mut stop_reason = String::from("end_turn");

        let mut stream = resp.bytes_stream();
        let mut buffer = String::new();
        use futures::StreamExt;

        let idle_dur = if self.stream_idle_secs == 0 {
            None
        } else {
            Some(std::time::Duration::from_secs(self.stream_idle_secs))
        };

        loop {
            let next_res = match idle_dur {
                Some(d) => match tokio::time::timeout(d, stream.next()).await {
                    Ok(v) => v,
                    Err(_) => bail!(
                        "stream_idle_timeout: no chunk for {}s",
                        self.stream_idle_secs
                    ),
                },
                None => stream.next().await,
            };
            let Some(chunk) = next_res else { break };
            let chunk = chunk?;
            buffer.push_str(&String::from_utf8_lossy(&chunk));

            while let Some(pos) = buffer.find("\n\n") {
                let event_str = buffer[..pos].to_string();
                buffer = buffer[pos + 2..].to_string();

                if let Some(data_line) = event_str.lines().find(|l| l.starts_with("data: ")) {
                    let json_str = &data_line[6..];
                    if let Ok(data) = serde_json::from_str::<serde_json::Value>(json_str) {
                        match data["type"].as_str() {
                            Some("content_block_delta") => {
                                if let Some(text) = data["delta"]["text"].as_str() {
                                    full_text.push_str(text);
                                    let _ = tx
                                        .send(StreamChunk {
                                            kind: StreamChunkKind::TextDelta,
                                            content: text.to_string(),
                                        })
                                        .await;
                                }
                            }
                            Some("message_delta") => {
                                if let Some(sr) = data["delta"]["stop_reason"].as_str() {
                                    stop_reason = sr.to_string();
                                }
                                if let Some(out) = data["usage"]["output_tokens"].as_u64() {
                                    usage.output_tokens = out;
                                }
                            }
                            Some("message_start") => {
                                if let Some(inp) =
                                    data["message"]["usage"]["input_tokens"].as_u64()
                                {
                                    usage.input_tokens = inp;
                                }
                            }
                            _ => {}
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

        Ok(LlmResponse {
            content: vec![ContentBlock::Text { text: full_text }],
            stop_reason,
            usage,
        })
    }

    fn estimate_cost(&self, model: &str, input_tokens: u64, output_tokens: u64) -> f64 {
        let (input_rate, output_rate) = match model {
            m if m.contains("opus") => (15.0, 75.0),
            m if m.contains("sonnet") => (3.0, 15.0),
            m if m.contains("haiku") => (0.25, 1.25),
            _ => (3.0, 15.0),
        };
        (input_tokens as f64 * input_rate + output_tokens as f64 * output_rate) / 1_000_000.0
    }
}
