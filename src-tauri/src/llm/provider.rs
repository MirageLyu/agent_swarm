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
}
