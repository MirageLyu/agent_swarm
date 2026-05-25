use std::collections::BTreeMap;

use super::types::{BenchmarkMetrics, BenchmarkResult};
use crate::db::queries::EventRow;

#[derive(Debug, Clone, Default)]
pub struct CostRecordInput {
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cost_usd: f64,
}

pub fn extract_case_metrics(
    events: &[EventRow],
    costs: &[CostRecordInput],
    runtime_ms: Option<i64>,
) -> BenchmarkMetrics {
    let mut metrics = BenchmarkMetrics {
        runtime_ms,
        ..BenchmarkMetrics::default()
    };

    for cost in costs {
        metrics.input_tokens += cost.input_tokens;
        metrics.output_tokens += cost.output_tokens;
        metrics.cost_usd += cost.cost_usd;
    }
    metrics.total_tokens = metrics.input_tokens + metrics.output_tokens;

    for event in events {
        match event.kind.as_str() {
            "llm_call" => metrics.llm_request_count += 1,
            "tool_use" => {
                metrics.tool_call_count += 1;
                if let Some(tool) = tool_name(event) {
                    *metrics.tool_call_count_by_name.entry(tool).or_insert(0) += 1;
                }
            }
            "tool_result" => {
                metrics.tool_result_count += 1;
                if event_is_error(event) {
                    metrics.tool_error_count += 1;
                }
            }
            "guardrail_fail" => metrics.guardrail_retry_count += 1,
            "recovery_attempt" => metrics.recovery_attempt_count += 1,
            "system_hint" => {
                if event.content.to_ascii_lowercase().contains("read-only")
                    || event.content.contains("只读")
                {
                    metrics.read_only_loop_hint_count += 1;
                }
            }
            _ => {}
        }
    }

    metrics.tool_error_rate = if metrics.tool_result_count > 0 {
        Some(metrics.tool_error_count as f64 / metrics.tool_result_count as f64)
    } else {
        None
    };
    metrics
}

pub fn aggregate_run_metrics(
    case_metrics: &[BenchmarkMetrics],
    results: &[BenchmarkResult],
) -> BenchmarkMetrics {
    let mut aggregate = BenchmarkMetrics::default();
    for metrics in case_metrics {
        aggregate.input_tokens += metrics.input_tokens;
        aggregate.output_tokens += metrics.output_tokens;
        aggregate.total_tokens += metrics.total_tokens;
        aggregate.cost_usd += metrics.cost_usd;
        aggregate.llm_request_count += metrics.llm_request_count;
        aggregate.tool_call_count += metrics.tool_call_count;
        aggregate.tool_result_count += metrics.tool_result_count;
        aggregate.tool_error_count += metrics.tool_error_count;
        aggregate.guardrail_retry_count += metrics.guardrail_retry_count;
        aggregate.recovery_attempt_count += metrics.recovery_attempt_count;
        aggregate.read_only_loop_hint_count += metrics.read_only_loop_hint_count;
        aggregate.runtime_ms =
            Some(aggregate.runtime_ms.unwrap_or(0) + metrics.runtime_ms.unwrap_or(0));
        merge_tool_counts(
            &mut aggregate.tool_call_count_by_name,
            &metrics.tool_call_count_by_name,
        );
    }

    let total = results.len() as i64;
    let successful = results.iter().filter(|r| r.success == Some(true)).count() as i64;
    let graded = results
        .iter()
        .filter(|r| matches!(r.success, Some(true) | Some(false)))
        .count() as i64;
    let graded_successful = results.iter().filter(|r| r.success == Some(true)).count() as i64;

    aggregate.total_case_count = Some(total);
    aggregate.successful_case_count = Some(successful);
    aggregate.graded_case_count = Some(graded);
    aggregate.all_cases_tsr = if total > 0 {
        Some(successful as f64 / total as f64)
    } else {
        None
    };
    aggregate.graded_cases_tsr = if graded > 0 {
        Some(graded_successful as f64 / graded as f64)
    } else {
        None
    };
    aggregate.token_per_success = if successful > 0 {
        Some(aggregate.total_tokens as f64 / successful as f64)
    } else {
        None
    };
    aggregate.tool_calls_per_success = if successful > 0 {
        Some(aggregate.tool_call_count as f64 / successful as f64)
    } else {
        None
    };
    aggregate.requests_per_success = if successful > 0 {
        Some(aggregate.llm_request_count as f64 / successful as f64)
    } else {
        None
    };
    aggregate.tool_error_rate = if aggregate.tool_result_count > 0 {
        Some(aggregate.tool_error_count as f64 / aggregate.tool_result_count as f64)
    } else {
        None
    };
    aggregate
}

fn merge_tool_counts(target: &mut BTreeMap<String, i64>, source: &BTreeMap<String, i64>) {
    for (name, count) in source {
        *target.entry(name.clone()).or_insert(0) += count;
    }
}

fn tool_name(event: &EventRow) -> Option<String> {
    event
        .meta
        .as_deref()
        .and_then(|m| serde_json::from_str::<serde_json::Value>(m).ok())
        .and_then(|v| {
            v.get("tool")
                .or_else(|| v.get("name"))
                .and_then(|s| s.as_str())
                .map(str::to_string)
        })
        .or_else(|| {
            event
                .content
                .split_whitespace()
                .next()
                .map(str::to_string)
                .filter(|s| !s.is_empty())
        })
}

fn event_is_error(event: &EventRow) -> bool {
    if let Some(meta) = &event.meta {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(meta) {
            if value
                .get("is_error")
                .or_else(|| value.get("isError"))
                .and_then(|v| v.as_bool())
                == Some(true)
            {
                return true;
            }
        }
    }
    let content = event.content.to_ascii_lowercase();
    content.contains("error") || content.contains("failed") || content.contains("exception")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::queries::EventRow;

    fn event(kind: &str, content: &str, meta: Option<&str>) -> EventRow {
        EventRow {
            id: format!("{kind}-id"),
            agent_id: "a1".to_string(),
            step: 1,
            kind: kind.to_string(),
            content: content.to_string(),
            meta: meta.map(str::to_string),
            created_at: "2026-05-22 00:00:00".to_string(),
        }
    }

    #[test]
    fn extracts_tool_and_token_metrics() {
        let events = vec![
            event("llm_call", "call", None),
            event("tool_use", "grep", Some(r#"{"tool":"grep"}"#)),
            event("tool_result", "ok", Some(r#"{"is_error":false}"#)),
            event("tool_use", "shell_exec", Some(r#"{"tool":"shell_exec"}"#)),
            event("tool_result", "error: failed", Some(r#"{"is_error":true}"#)),
            event("recovery_attempt", "retry", None),
        ];
        let costs = vec![CostRecordInput {
            input_tokens: 100,
            output_tokens: 40,
            cost_usd: 0.01,
        }];

        let metrics = extract_case_metrics(&events, &costs, Some(123));
        assert_eq!(metrics.total_tokens, 140);
        assert_eq!(metrics.llm_request_count, 1);
        assert_eq!(metrics.tool_call_count, 2);
        assert_eq!(metrics.tool_error_count, 1);
        assert_eq!(metrics.tool_call_count_by_name.get("grep"), Some(&1));
        assert_eq!(metrics.recovery_attempt_count, 1);
        assert_eq!(metrics.tool_error_rate, Some(0.5));
    }

    #[test]
    fn token_per_success_is_none_without_successes() {
        let results = vec![BenchmarkResult {
            id: "r1".into(),
            run_id: "run".into(),
            case_id: "c1".into(),
            agent_id: None,
            workspace_path: None,
            status: "completed".into(),
            success: Some(false),
            grading_status: "passed".into(),
            final_response: None,
            artifact_refs: vec![],
            error_message: None,
            started_at: None,
            completed_at: None,
            created_at: String::new(),
            updated_at: String::new(),
        }];
        let aggregate = aggregate_run_metrics(
            &[BenchmarkMetrics {
                total_tokens: 100,
                ..BenchmarkMetrics::default()
            }],
            &results,
        );
        assert_eq!(aggregate.token_per_success, None);
        assert_eq!(aggregate.all_cases_tsr, Some(0.0));
    }
}
