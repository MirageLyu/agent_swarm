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

/// Sentinel key 标记"LLM 这次 tool_use 的 arguments 解析失败 / 为空"。
/// AgentEngine::dispatch_tool 入口看到 input 含该 key 时直接返回结构化错误，
/// 让 LLM 看见自己漏写了参数（而不是误以为 schema 错），下一轮才能修正。
///
/// 修复的真实场景：DeepSeek / Qwen 等 OpenAI-compat provider 在并行 tool call、
/// max_tokens 截断、或某些边角条件下，会发出 `function.arguments == ""` 或非法 JSON 的
/// tool_call。旧实现静默 fallback 到 `{}` 并继续投喂工具，工具立即 "Missing 'path' parameter"
/// → LLM 看不出是自己漏写、又重试一次→ 同样错误循环。
pub const ARG_PARSE_ERROR_KEY: &str = "__arg_parse_error__";
pub const ARG_RAW_KEY: &str = "__raw_args__";

/// 解析 OpenAI/DeepSeek 风格 tool call 的 arguments 字符串为 JSON Value。
/// 空字符串 / 解析失败时返回 sentinel input 而不是默默吞错。
pub(crate) fn parse_tool_arguments_or_sentinel(
    tool_name: &str,
    args_str: &str,
) -> serde_json::Value {
    let trimmed = args_str.trim();
    if trimmed.is_empty() {
        tracing::warn!(
            "tool_use `{tool_name}`: provider returned empty arguments string; \
             surfacing as parse-error sentinel so the agent can self-correct."
        );
        return json!({
            ARG_PARSE_ERROR_KEY: "empty arguments string from provider",
            ARG_RAW_KEY: "",
        });
    }
    match serde_json::from_str::<serde_json::Value>(trimmed) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                "tool_use `{tool_name}`: arguments JSON parse failed: {e} | raw: {args_str}"
            );
            json!({
                ARG_PARSE_ERROR_KEY: format!("invalid JSON: {e}"),
                ARG_RAW_KEY: args_str,
            })
        }
    }
}

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

                    // **DeepSeek/OpenAI tool-call 协议合规关键**：tool_results 必须**紧接**
                    // 前一条 assistant tool_calls，中间不能塞 role=user。所以这里先 extend
                    // tool_results，再 push 任何 user text。
                    //
                    // 早期版本反过来（先 user text 再 tool）触发了 DeepSeek-V4 reseller 的
                    // `insufficient tool messages following tool_calls message` 400，
                    // stream-retry 5 次都救不回来。详见 ToolFollowupBuilder 文档。
                    let has_tool_results = !tool_results.is_empty();
                    oai_messages.extend(tool_results);

                    if !text_parts.is_empty() {
                        let mut user_msg = json!({
                            "role": "user",
                            "content": text_parts.join("\n"),
                        });
                        if msg.cache_control.is_some() {
                            user_msg["cache_control"] = json!({"type": "ephemeral"});
                        }
                        oai_messages.push(user_msg);
                    } else if !has_tool_results && msg.cache_control.is_some() {
                        // 既无文本又无工具结果但带了 cache_control（极小概率）——
                        // 保持旧行为：这种空消息直接被丢弃，不引入空的 user_msg。
                    }
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
        // Provider-specific 透传：把 LlmRequest.provider_extras 顶层 key/value
        // merge 进 body。冲突时 extras 覆盖（让 caller 拥有最终决定权——例如
        // 显式覆盖 max_tokens 之类的边角调优）。
        // 典型用途：DeepSeek-V4 系列的 `thinking: {"type": "disabled"}`。
        // 见 [`crate::llm::deepseek_adapter`]。
        if let Some(extras) = &request.provider_extras {
            if let Some(extras_obj) = extras.as_object() {
                if let Some(body_obj) = body.as_object_mut() {
                    for (k, v) in extras_obj {
                        body_obj.insert(k.clone(), v.clone());
                    }
                }
            } else {
                tracing::warn!(
                    extras = %extras,
                    "provider_extras is not a JSON object; ignored. \
                     Only top-level object merge is supported."
                );
            }
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
                let args_str = tc["function"]["arguments"].as_str().unwrap_or("");
                let input = parse_tool_arguments_or_sentinel(&name, args_str);
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

    fn endpoint_hint(&self) -> Option<String> {
        Some(self.endpoint())
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
        let stream_started = std::time::Instant::now();
        let endpoint = self.endpoint();

        tracing::debug!(
            provider = "openai_compat",
            endpoint = %endpoint,
            model = %request.model,
            msg_count = request.messages.len(),
            tool_count = request.tools.len(),
            max_tokens = request.max_tokens,
            "stream_chat sending HTTP POST"
        );

        let resp = self
            .client
            .post(&endpoint)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        let connect_ms = stream_started.elapsed().as_millis() as u64;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            tracing::warn!(
                provider = "openai_compat",
                model = %request.model,
                connect_ms,
                http_status = %status,
                body_excerpt = %text.chars().take(400).collect::<String>(),
                "stream_chat HTTP error"
            );
            bail!("OpenAI compat API error {status}: {text}");
        }
        tracing::info!(
            provider = "openai_compat",
            model = %request.model,
            connect_ms,
            "stream_chat HTTP connected, awaiting first chunk"
        );

        let mut full_text = String::new();
        let mut full_reasoning = String::new();
        let mut tool_calls: Vec<(String, String, String)> = Vec::new(); // (id, name, args)
        let mut usage = TokenUsage::default();
        let mut finish_reason = String::from("stop");

        let mut stream = resp.bytes_stream();
        let mut buffer = String::new();
        let mut first_byte_logged = false;
        let mut total_bytes: u64 = 0;
        // Stall 复盘用的窗口统计：每收到 byte 时累加，每 30s 边界时落一条 progress 日志，
        // chunk 间隔 >=5s 时单独 warn 标记"开始变慢的精确时刻"。这两条与 stream_guard
        // 在 chunk 级别的 gap warn 互补：byte 级看 raw socket 抵达节奏，chunk 级看 SSE
        // event 解析节奏；如果 byte 在抵达但 chunk 不增长，说明 reseller 在发"心跳/空白行"
        // 而非真内容。
        let mut last_byte_at = std::time::Instant::now();
        let mut window_start = std::time::Instant::now();
        let mut window_bytes: u64 = 0;
        let mut window_events: u64 = 0;
        let mut window_idx: u64 = 0;
        const WINDOW_SECS: u64 = 30;
        const BYTE_GAP_WARN_MS: u128 = 5_000;
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
                        // 与下面的 stream-error fallback 同语义：已经有任何内容就当
                        // 自然结束，否则真 idle bail。判定字段对齐：text / reasoning /
                        // tool_calls 任一非空即视为"已经吐过有用产物"。
                        let has_partial = !full_text.is_empty()
                            || !full_reasoning.is_empty()
                            || !tool_calls.is_empty();
                        if has_partial {
                            tracing::warn!(
                                provider = "openai_compat",
                                model = %request.model,
                                idle_secs = secs,
                                text_chars = full_text.len(),
                                reasoning_chars = full_reasoning.len(),
                                tool_call_count = tool_calls.len(),
                                "LLM stream idle after receiving partial content; returning what we have"
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
                    // **SSE 协议适配**：HTML5 SSE 标准（WHATWG）明确说 server-initiated
                    // connection close **即** stream 自然结束。但 hyper 的 chunked decoder
                    // 严格判定 transfer-encoding 完整性，server 没发 final `0\r\n\r\n` chunk
                    // 就 close 时会报 `error decoding response body` —— 这在通用 HTTP 场景
                    // 是 bug，但在 SSE 上下文是合法行为。所以这里 client 侧负责"按 SSE 协议
                    // 重新解释"：只要已经吐出过任何 SSE event，就把 stream end 当成"自然结束
                    // + finish_reason 缺失"处理，由调用方按 stop_reason="stop" 走下一步。
                    //
                    // 检测条件：text / reasoning / tool_calls 任一已有内容。三者覆盖了
                    // OpenAI-compat 协议下 stream 能产出的所有 assistant 内容形态：
                    //   - text:        普通模型的 content delta
                    //   - reasoning:   DeepSeek-R1/V4、QwQ 等推理模型的 reasoning_content
                    //   - tool_calls:  function calling 累积的 args（即使只有一半也算见过 chunk）
                    //
                    // 只有完全没有任何 chunk 抵达就断的情况，才算真正的连接失败 → bail。
                    let has_partial =
                        !full_text.is_empty() || !full_reasoning.is_empty() || !tool_calls.is_empty();
                    if has_partial {
                        tracing::info!(
                            provider = "openai_compat",
                            model = %request.model,
                            text_chars = full_text.len(),
                            reasoning_chars = full_reasoning.len(),
                            tool_call_count = tool_calls.len(),
                            total_bytes = total_bytes,
                            elapsed_ms = stream_started.elapsed().as_millis() as u64,
                            error = %e,
                            "SSE stream ended via connection close; treating as natural end-of-stream per SSE protocol (server may have omitted final chunked terminator)"
                        );
                        // 缺失 finish_reason 时强制 "stop"——下一步 engine 看到 stop=stop
                        // + 空/部分 content 会按既有的"无 tool_call 则督促继续"流程自然续上，
                        // 不需要任何上层特殊路径。
                        if finish_reason == "stop" || finish_reason.is_empty() {
                            finish_reason = "stop".to_string();
                        }
                        break;
                    }
                    bail!("网络连接中断，请检查网络后重试 (stream error: {e})");
                }
            };
            let now = std::time::Instant::now();
            let byte_gap = now.duration_since(last_byte_at).as_millis();
            if first_byte_logged && byte_gap >= BYTE_GAP_WARN_MS {
                // **关键诊断点**：byte 级 socket recv 出现 5s+ 间隙 = 网络/上游
                // 在这个时间点开始向我们"喂得变慢"。配合 stall_diagnostic 的探测结果
                // 能锁定故障开始时刻。
                tracing::warn!(
                    provider = "openai_compat",
                    model = %request.model,
                    byte_gap_ms = byte_gap as u64,
                    total_bytes_so_far = total_bytes,
                    elapsed_ms = stream_started.elapsed().as_millis() as u64,
                    "stream_chat detected long byte-level gap on socket (>=5s); upstream/network slowing down"
                );
            }
            last_byte_at = now;
            total_bytes += chunk.len() as u64;
            window_bytes += chunk.len() as u64;
            // 每 WINDOW_SECS 落一条吞吐快照，方便事后对照故障时刻 throughput 是否塌方。
            // 用 elapsed >= 而不是 == 避免某次 recv 跨过窗口边界丢失采样。
            if window_start.elapsed().as_secs() >= WINDOW_SECS {
                window_idx += 1;
                let win_ms = window_start.elapsed().as_millis().max(1) as u64;
                let bps = (window_bytes * 1000) / win_ms;
                tracing::info!(
                    provider = "openai_compat",
                    model = %request.model,
                    window_idx,
                    window_secs = win_ms / 1000,
                    window_bytes,
                    window_events,
                    bytes_per_sec = bps,
                    total_bytes_so_far = total_bytes,
                    elapsed_ms = stream_started.elapsed().as_millis() as u64,
                    "stream_chat throughput window"
                );
                window_start = now;
                window_bytes = 0;
                window_events = 0;
            }
            if !first_byte_logged {
                first_byte_logged = true;
                tracing::info!(
                    provider = "openai_compat",
                    model = %request.model,
                    first_byte_ms = stream_started.elapsed().as_millis() as u64,
                    "stream_chat first SSE bytes received"
                );
            }
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
                    window_events += 1;
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
                                // Single-Agent Uplift P1-1: 流式 emit per-tool_use chunk
                                //
                                // 时序约束：必须先有 id（用于 ToolUseStart）才能 emit InputDelta，
                                // 否则 downstream executor 无法定位累积桶。OpenAI 协议**通常**
                                // 在第一个 tool_call delta 里就给 id+name，但不保证——做防御：
                                //   - 没 id 时 InputDelta 也累积进 tool_calls[idx].2，但不 emit
                                //     （fallback：finish_reason 后从 LlmResponse.content 解析）
                                //   - 第一次拿到 id 时立即 emit ToolUseStart，把"在此之前累积过
                                //     args"的情况合并补一条 InputDelta，给 downstream 完整 input
                                let mut first_id_seen_this_delta = false;
                                if let Some(id) = tc["id"].as_str() {
                                    if tool_calls[idx].0.is_empty() && !id.is_empty() {
                                        tool_calls[idx].0 = id.to_string();
                                        first_id_seen_this_delta = true;
                                    }
                                }
                                if let Some(name) = tc["function"]["name"].as_str() {
                                    tool_calls[idx].1.push_str(name);
                                }
                                // ToolUseStart：id+name 都 ready 时 emit
                                // 注意：name 可能跨 chunk 累积，等 .1 非空再 emit 反而更准
                                if first_id_seen_this_delta && !tool_calls[idx].1.is_empty() {
                                    let _ = tx
                                        .send(StreamChunk {
                                            kind: StreamChunkKind::ToolUseStart {
                                                tool_use_id: tool_calls[idx].0.clone(),
                                                name: tool_calls[idx].1.clone(),
                                            },
                                            content: String::new(),
                                        })
                                        .await;
                                }
                                if let Some(args) = tc["function"]["arguments"].as_str() {
                                    tool_calls[idx].2.push_str(args);
                                    // InputDelta：仅在 id 已知时 emit；否则 args 进累积器，
                                    // 等 finish_reason 走 fallback。这个分支理论上极少触发——
                                    // OpenAI/DeepSeek/Qwen 三家实测都是首个 chunk 即给 id。
                                    if !tool_calls[idx].0.is_empty() && !args.is_empty() {
                                        let _ = tx
                                            .send(StreamChunk {
                                                kind: StreamChunkKind::ToolUseInputDelta {
                                                    tool_use_id: tool_calls[idx].0.clone(),
                                                },
                                                content: args.to_string(),
                                            })
                                            .await;
                                    }
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

        // Single-Agent Uplift P1-1: OpenAI 协议无 per-tool_call stop 信号 ——
        // finish_reason 抵达后，主动给每个 tool_call emit ToolUseStop。
        // 严格按收到顺序（vec index），让 downstream executor 知道 input
        // 闭合可以触发 dispatch。空 id 的 slot 跳过（视为 fallback 走 LlmResponse）。
        for (id, _name, _args) in &tool_calls {
            if !id.is_empty() {
                let _ = tx
                    .send(StreamChunk {
                        kind: StreamChunkKind::ToolUseStop {
                            tool_use_id: id.clone(),
                        },
                        content: String::new(),
                    })
                    .await;
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
                let input = parse_tool_arguments_or_sentinel(&name, &args);
                content.push(ContentBlock::ToolUse { id, name, input });
            }
        }

        let stop_reason = match finish_reason.as_str() {
            "tool_calls" => "tool_use".to_string(),
            other => other.to_string(),
        };

        let total_ms = stream_started.elapsed().as_millis() as u64;
        tracing::info!(
            provider = "openai_compat",
            model = %request.model,
            total_ms,
            total_bytes,
            stop_reason = %stop_reason,
            input_tokens = usage.input_tokens,
            output_tokens = usage.output_tokens,
            tool_calls = content
                .iter()
                .filter(|b| matches!(b, ContentBlock::ToolUse { .. }))
                .count(),
            "stream_chat finished"
        );

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
            provider_extras: None,
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
            provider_extras: None,
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
            provider_extras: None,
        };

        let oai = provider().convert_messages(&req);
        assert_eq!(oai[0]["reasoning_content"], "part1 part2");
    }

    /// **provider_extras 透传**：DeepSeek-V4 的 thinking-off 适配靠它，
    /// 必须 merge 进 OpenAI body 顶层；非 object 值要安全忽略。
    #[test]
    fn build_body_merges_provider_extras_into_top_level() {
        let mut req = LlmRequest {
            model: "deepseek-v4-flash".into(),
            system: None,
            messages: vec![Message {
                role: MessageRole::User,
                content: vec![ContentBlock::Text { text: "hi".into() }],
                cache_control: None,
            }],
            tools: vec![],
            max_tokens: 100,
            provider_extras: Some(json!({
                "thinking": {"type": "disabled"},
                "top_p": 0.5
            })),
        };

        let body = provider().build_body(&req, false);
        assert_eq!(body["thinking"]["type"], "disabled");
        assert_eq!(body["top_p"], json!(0.5));
        // 已存在的字段不能被覆盖到错值
        assert_eq!(body["model"], "deepseek-v4-flash");
        assert_eq!(body["max_tokens"], 100);

        // None 时不引入任何 extras key
        req.provider_extras = None;
        let body2 = provider().build_body(&req, false);
        assert!(body2.get("thinking").is_none());
        assert!(body2.get("top_p").is_none());

        // 非 object（数组/数字/字符串）安全忽略，不 panic
        req.provider_extras = Some(json!([1, 2, 3]));
        let body3 = provider().build_body(&req, false);
        assert!(body3.get("thinking").is_none());
        assert_eq!(body3["model"], "deepseek-v4-flash");
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

    /// **回归测试 / FM-stream-eof**：复现 Bitfun reseller (nginx 1.24, HTTP/1.1) 在
    /// SSE stream 末段不发 final chunked terminator 就关闭 connection 的实际故障。
    ///
    /// 在没有这条修复前，hyper 报 `error decoding response body`，stream_chat 直接
    /// bail，agent 失败。修复后：按 SSE 协议把已收到的 reasoning_content 当作
    /// 自然结束，返回包含已收内容的 LlmResponse。
    ///
    /// **构造方式**：本地起 TcpListener（HTTP，不走 TLS——简化测试），写一段合法的
    /// HTTP/1.1 chunked SSE 响应 + 几个 deepseek 风格的 reasoning_content delta，
    /// 然后**直接 close socket** 不发 `0\r\n\r\n` 终止 chunk。这正是实际生产 log 里
    /// `error decoding response body` 的成因。
    #[tokio::test]
    async fn stream_chat_handles_truncated_chunked_body_per_sse_protocol() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;
        // AsyncWriteExt 在两处需要：mock server 主循环（write_all/flush）和最后的 shutdown。
        // 单点 use 表达更清晰，让 rustc 看到 trait 在 scope 里。

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        // 假 server：发 200 + chunked SSE → 几个 reasoning delta → 不发 final 0 chunk 直接 drop
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            // 读 request 头（不解析，只跳过到 \r\n\r\n）
            let mut buf = [0u8; 4096];
            let mut total = String::new();
            loop {
                let n = sock.read(&mut buf).await.unwrap_or(0);
                if n == 0 {
                    return;
                }
                total.push_str(&String::from_utf8_lossy(&buf[..n]));
                if total.contains("\r\n\r\n") {
                    break;
                }
            }
            // 写 response 头部 + 部分 chunked body
            let head = "HTTP/1.1 200 OK\r\n\
                        Content-Type: text/event-stream\r\n\
                        Transfer-Encoding: chunked\r\n\
                        Connection: close\r\n\r\n";
            sock.write_all(head.as_bytes()).await.unwrap();

            let send_chunk = |body: &str| -> Vec<u8> {
                let mut v = Vec::new();
                v.extend_from_slice(format!("{:x}\r\n", body.len()).as_bytes());
                v.extend_from_slice(body.as_bytes());
                v.extend_from_slice(b"\r\n");
                v
            };

            // 三段 reasoning_content delta（mimic 实际故障：30 chunks 119 bytes 这种短量级）
            for piece in ["Let", " me", " think"].iter() {
                let line = format!(
                    "data: {{\"choices\":[{{\"delta\":{{\"reasoning_content\":\"{piece}\"}}}}]}}\n\n"
                );
                sock.write_all(&send_chunk(&line)).await.unwrap();
                sock.flush().await.unwrap();
            }
            // **关键**：故意不发 `0\r\n\r\n` 终止 chunk。但要确保所有已写 chunk 都被对端
            // 收到——显式 shutdown(write)，发 FIN 前 flush；再短 sleep 让 reqwest read 完
            // 已传输数据；最后才 drop 让 reqwest 看到 connection close。
            // 这样测试覆盖的是"server 发完了内容但没发 chunked 终止帧 + 关连接"，正是
            // 实际故障 nginx 1.24 的行为。
            let _ = sock.shutdown().await;
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            drop(sock);
        });

        // stream_idle_secs=2：当 hyper 没立刻报 chunked-EOF（不同 hyper 版本行为略不同）时，
        // 2 秒 idle 兜底也能走到 partial fallback 路径，确保测试不会卡 30 分钟（client timeout）。
        let p = OpenAICompatProvider::with_stream_idle(
            "k".into(),
            format!("http://127.0.0.1:{port}/v1"),
            2,
        );
        let req = LlmRequest {
            model: "deepseek-v4-pro".into(),
            system: None,
            messages: vec![Message {
                role: MessageRole::User,
                content: vec![ContentBlock::Text { text: "hi".into() }],
                cache_control: None,
            }],
            tools: vec![],
            max_tokens: 100,
            provider_extras: None,
        };
        let (tx, mut rx) = mpsc::channel::<StreamChunk>(16);

        // 收 chunks 的 sink；不主动 drop，让 stream_chat 自己结束
        let collector = tokio::spawn(async move {
            let mut got = String::new();
            while let Some(c) = rx.recv().await {
                if matches!(c.kind, StreamChunkKind::ReasoningDelta) {
                    got.push_str(&c.content);
                }
            }
            got
        });

        // 测试级硬超时：8 秒还没返回就当回归（生产 client.timeout 是 1800s，让测试自己包一层）
        let resp = tokio::time::timeout(std::time::Duration::from_secs(8), p.stream_chat(&req, tx))
            .await
            .expect("partial fallback 应在 server EOF / idle 后秒级返回，不应该等 client timeout")
            .expect("partial fallback 必须把已收到的内容当成功，不能 bail");
        let collected = collector.await.unwrap();

        // 已收到的 reasoning 必须完整保留
        assert_eq!(collected, "Let me think", "downstream chunks 必须保留");
        let reasoning_block = resp.content.iter().find_map(|b| match b {
            ContentBlock::Reasoning { text } => Some(text.as_str()),
            _ => None,
        });
        assert_eq!(
            reasoning_block,
            Some("Let me think"),
            "LlmResponse 必须含 partial reasoning（下一轮要按 deepseek 协议原样回传）"
        );
        // server 没发 finish_reason，按"自然结束"应当默认 stop
        assert_eq!(resp.stop_reason, "stop", "stop_reason 应默认为 stop 让 engine 走下一步");
    }
}
