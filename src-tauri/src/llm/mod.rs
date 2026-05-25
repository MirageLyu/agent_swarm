mod anthropic;
pub mod deepseek_adapter;
pub mod error_class;
pub(crate) mod openai_compat;
mod provider;
pub mod registry;
pub mod stream_diagnostics;
mod stream_guard;
mod types;

pub use anthropic::AnthropicProvider;
pub use error_class::{classify_llm_error, LlmErrorClass};
pub use openai_compat::{OpenAICompatProvider, ARG_PARSE_ERROR_KEY, ARG_RAW_KEY};
pub use provider::LlmProvider;
pub use stream_diagnostics::{StreamRegistry, StreamStats};
pub use stream_guard::{
    stream_chat_with_idle_guard, stream_chat_with_idle_guard_cancellable,
    stream_chat_with_idle_guard_full, StreamGuardError, StreamRetryPolicy,
    DEFAULT_STREAM_IDLE_HEARTBEAT_SECS, DEFAULT_STREAM_IDLE_TIMEOUT,
};
pub use types::*;
