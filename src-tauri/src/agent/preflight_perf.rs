use std::collections::BTreeSet;
use std::time::Instant;

use serde::Serialize;

use crate::llm::TokenUsage;

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct PreflightLlmTiming {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_activity_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttft_ms: Option<u64>,
    pub total_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_chunk_kind: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct PreflightPerfSummary {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backend_prepare_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub llm_first_activity_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub llm_ttft_ms: Option<u64>,
    pub llm_total_ms: u64,
    pub tool_processing_ms: u64,
    pub continuation_count: u32,
    pub turn_total_ms: u64,
    pub tool_names: Vec<String>,
    pub compaction_triggered: bool,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
}

#[derive(Debug, Clone)]
pub struct PreflightTurnTiming {
    started_at: Instant,
    backend_prepare_ms: Option<u64>,
    llm_first_activity_ms: Option<u64>,
    llm_ttft_ms: Option<u64>,
    llm_total_ms: u64,
    tool_processing_ms: u64,
    continuation_count: u32,
    tool_names: BTreeSet<String>,
    compaction_triggered: bool,
    usage: TokenUsage,
}

impl PreflightTurnTiming {
    pub fn new() -> Self {
        Self {
            started_at: Instant::now(),
            backend_prepare_ms: None,
            llm_first_activity_ms: None,
            llm_ttft_ms: None,
            llm_total_ms: 0,
            tool_processing_ms: 0,
            continuation_count: 0,
            tool_names: BTreeSet::new(),
            compaction_triggered: false,
            usage: TokenUsage::default(),
        }
    }

    pub fn mark_llm_request_start(&mut self) {
        if self.backend_prepare_ms.is_none() {
            self.backend_prepare_ms = Some(elapsed_ms_since(self.started_at));
        }
    }

    pub fn mark_compaction_triggered(&mut self) {
        self.compaction_triggered = true;
    }

    pub fn record_llm_call(&mut self, timing: PreflightLlmTiming, usage: TokenUsage) {
        if self.llm_first_activity_ms.is_none() {
            self.llm_first_activity_ms = timing.first_activity_ms;
        }
        if self.llm_ttft_ms.is_none() {
            self.llm_ttft_ms = timing.ttft_ms;
        }
        self.llm_total_ms = self.llm_total_ms.saturating_add(timing.total_ms);
        self.usage.input_tokens = self.usage.input_tokens.saturating_add(usage.input_tokens);
        self.usage.output_tokens = self.usage.output_tokens.saturating_add(usage.output_tokens);
        self.usage.cache_read_input_tokens = self
            .usage
            .cache_read_input_tokens
            .saturating_add(usage.cache_read_input_tokens);
        self.usage.cache_creation_input_tokens = self
            .usage
            .cache_creation_input_tokens
            .saturating_add(usage.cache_creation_input_tokens);
    }

    pub fn record_tool_processing<I, S>(&mut self, duration_ms: u64, tool_names: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.tool_processing_ms = self.tool_processing_ms.saturating_add(duration_ms);
        self.tool_names
            .extend(tool_names.into_iter().map(Into::into));
    }

    pub fn record_continuation(&mut self) {
        self.continuation_count = self.continuation_count.saturating_add(1);
    }

    pub fn summary(&self) -> PreflightPerfSummary {
        PreflightPerfSummary {
            backend_prepare_ms: self.backend_prepare_ms,
            llm_first_activity_ms: self.llm_first_activity_ms,
            llm_ttft_ms: self.llm_ttft_ms,
            llm_total_ms: self.llm_total_ms,
            tool_processing_ms: self.tool_processing_ms,
            continuation_count: self.continuation_count,
            turn_total_ms: elapsed_ms_since(self.started_at),
            tool_names: self.tool_names.iter().cloned().collect(),
            compaction_triggered: self.compaction_triggered,
            input_tokens: self.usage.input_tokens,
            output_tokens: self.usage.output_tokens,
            cache_read_input_tokens: self.usage.cache_read_input_tokens,
            cache_creation_input_tokens: self.usage.cache_creation_input_tokens,
        }
    }
}

impl Default for PreflightTurnTiming {
    fn default() -> Self {
        Self::new()
    }
}

pub fn elapsed_ms_since(started_at: Instant) -> u64 {
    u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::TokenUsage;
    use serde_json::json;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn llm_timing_serializes_missing_ttft_without_field() {
        let timing = PreflightLlmTiming {
            first_activity_ms: Some(12),
            ttft_ms: None,
            total_ms: 34,
            first_chunk_kind: Some("text_delta".to_string()),
        };

        let serialized = serde_json::to_value(timing).expect("serialize LLM timing");

        assert_eq!(serialized.get("first_activity_ms"), Some(&json!(12)));
        assert!(serialized.get("ttft_ms").is_none());
        assert_eq!(serialized.get("total_ms"), Some(&json!(34)));
        assert_eq!(
            serialized.get("first_chunk_kind"),
            Some(&json!("text_delta"))
        );
    }

    #[test]
    fn turn_summary_aggregates_tokens_tools_and_continuations() {
        let mut timing = PreflightTurnTiming::new();

        timing.mark_llm_request_start();
        timing.record_llm_call(
            PreflightLlmTiming {
                first_activity_ms: Some(10),
                ttft_ms: Some(20),
                total_ms: 100,
                first_chunk_kind: Some("text_delta".to_string()),
            },
            TokenUsage {
                input_tokens: 1_000,
                output_tokens: 100,
                cache_read_input_tokens: 400,
                cache_creation_input_tokens: 50,
            },
        );
        timing.record_continuation();
        timing.record_llm_call(
            PreflightLlmTiming {
                first_activity_ms: Some(30),
                ttft_ms: None,
                total_ms: 200,
                first_chunk_kind: Some("tool_use".to_string()),
            },
            TokenUsage {
                input_tokens: 2_000,
                output_tokens: 200,
                cache_read_input_tokens: 500,
                cache_creation_input_tokens: 60,
            },
        );
        timing.record_tool_processing(
            75,
            [
                "present_choices".to_string(),
                "add_contract_item".to_string(),
            ],
        );
        timing.record_tool_processing(25, ["present_choices".to_string()]);
        timing.mark_compaction_triggered();

        let summary = timing.summary();

        assert!(summary.backend_prepare_ms.is_some());
        assert_eq!(summary.llm_first_activity_ms, Some(10));
        assert_eq!(summary.llm_ttft_ms, Some(20));
        assert_eq!(summary.llm_total_ms, 300);
        assert_eq!(summary.tool_processing_ms, 100);
        assert_eq!(summary.continuation_count, 1);
        assert_eq!(summary.tool_names, ["add_contract_item", "present_choices"]);
        assert!(summary.compaction_triggered);
        assert_eq!(summary.input_tokens, 3_000);
        assert_eq!(summary.output_tokens, 300);
        assert_eq!(summary.cache_read_input_tokens, 900);
        assert_eq!(summary.cache_creation_input_tokens, 110);
    }

    #[test]
    fn llm_request_start_is_idempotent() {
        let mut timing = PreflightTurnTiming::new();

        timing.mark_llm_request_start();
        let first_backend_prepare_ms = timing
            .summary()
            .backend_prepare_ms
            .expect("first request start records backend prepare duration");

        thread::sleep(Duration::from_millis(5));
        timing.mark_llm_request_start();
        let second_backend_prepare_ms = timing
            .summary()
            .backend_prepare_ms
            .expect("repeated request start keeps backend prepare duration");

        assert_eq!(second_backend_prepare_ms, first_backend_prepare_ms);
    }
}
