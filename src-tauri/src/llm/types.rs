use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: MessageRole,
    pub content: Vec<ContentBlock>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    User,
    Assistant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: String,
        is_error: bool,
    },
    /// Reasoning / "thinking" 内容。
    ///
    /// 某些 OpenAI-compat 推理模型（DeepSeek-R1、DeepSeek V4 系列、QwQ 等）
    /// 在响应里返回 `reasoning_content` 字段。**下一轮请求必须把它原样回传**，
    /// 否则 API 会返回 400 "The reasoning_content in the thinking mode must
    /// be passed back to the API."。
    ///
    /// 设计为独立 variant 而不是塞进 Text：
    /// - 业务逻辑（agent loop / tool dispatch）只关心 Text + ToolUse，
    ///   Reasoning 在所有现有 match 的 `_ => {}` 兜底里被自然忽略
    /// - openai_compat 在 convert_messages 时把 Reasoning 块合并进 assistant
    ///   message 的 `reasoning_content` 字段，专门解决 round-trip 协议
    /// - anthropic 在 build_body 时过滤掉（Anthropic 有自己的 thinking 块协议，
    ///   schema 与 OpenAI compat 不同，目前未启用）
    #[serde(rename = "reasoning")]
    Reasoning { text: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheControl {
    #[serde(rename = "type")]
    pub control_type: String,
}

impl CacheControl {
    pub fn ephemeral() -> Self {
        Self { control_type: "ephemeral".to_string() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmResponse {
    pub content: Vec<ContentBlock>,
    pub stop_reason: String,
    pub usage: TokenUsage,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
}

impl TokenUsage {
    pub fn cache_hit_ratio(&self) -> f64 {
        if self.input_tokens == 0 { return 0.0; }
        self.cache_read_input_tokens as f64 / self.input_tokens as f64
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamChunk {
    pub kind: StreamChunkKind,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StreamChunkKind {
    TextDelta,
    ReasoningDelta,
    ToolUseStart { id: String, name: String },
    ToolUseInputDelta,
    MessageStop,
}

#[derive(Debug, Clone)]
pub struct LlmRequest {
    pub model: String,
    pub system: Option<String>,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDefinition>,
    pub max_tokens: u32,
    /// Provider-specific 顶层透传字段。
    ///
    /// 当 provider 是 OpenAI-compat 时，[`crate::llm::OpenAICompatProvider::build_body`]
    /// 会把这个 JSON object 的所有顶层 key/value **merge** 进请求体。其它 provider
    /// 暂时忽略。
    ///
    /// **典型用途**：DeepSeek-V4 系列 reasoning model 的"关 thinking" 适配 ——
    /// `Some(json!({"thinking": {"type": "disabled"}}))` 传进去后，bitfun reseller
    /// 实测延迟从 7s 降到 1.5s（2026-05-18 dial-test 数据），同时拿到完整 content。
    /// 见 [`crate::llm::deepseek_adapter`]。
    ///
    /// 设计上故意做成"opaque JSON"而不是强类型枚举：每家 provider 的扩展参数
    /// 都不一样（DeepSeek thinking、Anthropic thinking、Qwen enable_thinking、
    /// vLLM chat_template_kwargs），强类型 union 会在每加一家就侵入到 LlmRequest。
    /// 让适配逻辑各自管自家 json 拼装，types.rs 保持中立。
    pub provider_extras: Option<serde_json::Value>,
}

// ---------------------------------------------------------------------------
// Model Capability Registry types (FM-10.3.9)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCapabilities {
    pub supports_thinking: bool,
    pub supports_tool_use: bool,
    pub supports_prompt_caching: bool,
    pub supports_prefill: bool,
    pub supports_streaming: bool,
    pub supports_parallel_tools: bool,
    pub supports_logprobs: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_api_param: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control_syntax: Option<String>,
    pub context_window: u64,
}

impl Default for ModelCapabilities {
    fn default() -> Self {
        Self {
            supports_thinking: false,
            supports_tool_use: false,
            supports_prompt_caching: false,
            supports_prefill: false,
            supports_streaming: true,
            supports_parallel_tools: false,
            supports_logprobs: false,
            thinking_api_param: None,
            cache_control_syntax: None,
            context_window: 32768,
        }
    }
}
