use anyhow::Result;
use async_trait::async_trait;
use tokio::sync::mpsc;

use super::types::{LlmRequest, LlmResponse, StreamChunk};

#[async_trait]
pub trait LlmProvider: Send + Sync {
    fn name(&self) -> &str;
    async fn chat(&self, request: &LlmRequest) -> Result<LlmResponse>;
    async fn stream_chat(
        &self,
        request: &LlmRequest,
        tx: mpsc::Sender<StreamChunk>,
    ) -> Result<LlmResponse>;
    fn estimate_cost(&self, model: &str, input_tokens: u64, output_tokens: u64) -> f64;

    /// **诊断**：返回 stream_chat 实际请求的完整 URL，用于：
    /// 1. `StreamRegistry` 注册时给条目带上 endpoint，stall 时 dump 一目了然
    /// 2. 在 `stream_guard` 检测到 stall 时启动 in-process probe，向同 host
    ///    发轻量 GET 测网络层活性
    ///
    /// 默认 `None`：旧 provider 不实现也能编译；`OpenAICompatProvider` 和
    /// `AnthropicProvider` 各自覆写返回 `chat/completions` 或 `messages` 的真实 URL。
    fn endpoint_hint(&self) -> Option<String> {
        None
    }
}
