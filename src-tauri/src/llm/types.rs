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
