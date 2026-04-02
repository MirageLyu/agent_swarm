use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: MessageRole,
    pub content: Vec<ContentBlock>,
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamChunk {
    pub kind: StreamChunkKind,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StreamChunkKind {
    TextDelta,
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
