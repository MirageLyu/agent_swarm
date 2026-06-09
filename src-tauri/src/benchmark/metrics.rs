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
                if event.content.starts_with("[evidence_read_ref]") {
                    metrics.evidence_read_ref_count += 1;
                }
                if is_shell_content_command(event) {
                    metrics.shell_content_command_count += 1;
                }
            }
            "tool_result_policy" => apply_tool_result_policy_metrics(event, &mut metrics),
            "guardrail_fail" => metrics.guardrail_retry_count += 1,
            "contract_pass" => metrics.contract_validation_attempt_count += 1,
            "contract_fail" => {
                metrics.contract_validation_attempt_count += 1;
                metrics.contract_repair_retry_count += 1;
                metrics.contract_violation_count += contract_violation_count(event);
            }
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
        aggregate.context_saved_chars += metrics.context_saved_chars;
        aggregate.tool_result_ref_count += metrics.tool_result_ref_count;
        aggregate.tool_result_repeat_count += metrics.tool_result_repeat_count;
        aggregate.evidence_read_ref_count += metrics.evidence_read_ref_count;
        aggregate.shell_content_command_count += metrics.shell_content_command_count;
        aggregate.persisted_tool_result_count += metrics.persisted_tool_result_count;
        aggregate.per_message_budget_replacement_count +=
            metrics.per_message_budget_replacement_count;
        aggregate.contract_validation_attempt_count += metrics.contract_validation_attempt_count;
        aggregate.contract_violation_count += metrics.contract_violation_count;
        aggregate.contract_repair_retry_count += metrics.contract_repair_retry_count;
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

fn apply_tool_result_policy_metrics(event: &EventRow, metrics: &mut BenchmarkMetrics) {
    let Some(meta) = event_meta(event) else {
        return;
    };
    let saved_chars = meta
        .get("saved_chars")
        .or_else(|| meta.pointer("/context_policy/saved_chars"))
        .and_then(|v| v.as_i64());
    if let Some(saved_chars) = saved_chars {
        metrics.context_saved_chars += saved_chars.max(0);
    } else {
        let from_chars = meta
            .get("from_chars")
            .or_else(|| meta.pointer("/context_policy/original_chars"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let to_chars = meta
            .get("to_chars")
            .or_else(|| meta.pointer("/context_policy/context_chars"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        metrics.context_saved_chars += (from_chars - to_chars).max(0);
    }

    if meta
        .pointer("/context_policy/persisted_path")
        .or_else(|| meta.get("persisted_path"))
        .and_then(|v| v.as_str())
        .is_some()
    {
        metrics.persisted_tool_result_count += 1;
    }
    if meta
        .pointer("/context_policy/per_message_budget_replaced")
        .or_else(|| meta.get("per_message_budget_replaced"))
        .and_then(|v| v.as_bool())
        == Some(true)
    {
        metrics.per_message_budget_replacement_count += 1;
    }

    if let Some(mode) = meta
        .pointer("/context_policy/mode")
        .or_else(|| meta.get("mode"))
        .and_then(|v| v.as_str())
    {
        match mode {
            "evidence_ref" => metrics.tool_result_ref_count += 1,
            "repeat_ref" => metrics.tool_result_repeat_count += 1,
            _ => {}
        }
    }
}

fn is_shell_content_command(event: &EventRow) -> bool {
    let Some(meta) = event_meta(event) else {
        return false;
    };
    if meta.get("tool").and_then(|v| v.as_str()) != Some("shell_exec") {
        return false;
    }
    matches!(
        meta.pointer("/output/command_family")
            .and_then(|v| v.as_str()),
        Some("fetch" | "file_dump" | "file_excerpt" | "directory_listing")
    )
}

fn contract_violation_count(event: &EventRow) -> i64 {
    let Some(meta) = event_meta(event) else {
        return 0;
    };
    match meta {
        serde_json::Value::Array(items) => items
            .iter()
            .filter(|item| item.get("passed").and_then(|v| v.as_bool()) == Some(false))
            .count() as i64,
        _ => 0,
    }
}

fn event_meta(event: &EventRow) -> Option<serde_json::Value> {
    event
        .meta
        .as_deref()
        .and_then(|m| serde_json::from_str::<serde_json::Value>(m).ok())
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
    fn extracts_context_policy_and_shell_content_metrics() {
        let events = vec![
            event(
                "tool_result_policy",
                "Rendered shell_exec tool_result as evidence_ref",
                Some(
                    r#"{"saved_chars":4300,"context_policy":{"mode":"evidence_ref","persisted_path":"/tmp/tool-result.txt","per_message_budget_replaced":true}}"#,
                ),
            ),
            event(
                "tool_result_policy",
                "Rendered shell_exec tool_result as repeat_ref",
                Some(
                    r#"{"from_chars":5000,"to_chars":180,"context_policy":{"mode":"repeat_ref"}}"#,
                ),
            ),
            event(
                "tool_result",
                "[evidence_read_ref]\npath: evidence/stdout.txt",
                Some(r#"{"tool":"read_file","is_error":false}"#),
            ),
            event(
                "tool_result",
                "content",
                Some(
                    r#"{"tool":"shell_exec","is_error":false,"output":{"command_family":"fetch"}}"#,
                ),
            ),
        ];

        let metrics = extract_case_metrics(&events, &[], None);
        assert_eq!(metrics.context_saved_chars, 9120);
        assert_eq!(metrics.tool_result_ref_count, 1);
        assert_eq!(metrics.tool_result_repeat_count, 1);
        assert_eq!(metrics.evidence_read_ref_count, 1);
        assert_eq!(metrics.shell_content_command_count, 1);
        assert_eq!(metrics.persisted_tool_result_count, 1);
        assert_eq!(metrics.per_message_budget_replacement_count, 1);
    }

    #[test]
    fn extracts_contract_validation_metrics() {
        let events = vec![
            event(
                "contract_fail",
                "contract failed",
                Some(
                    r#"[{"name":"final_response","passed":false},{"name":"artifact_json","passed":false},{"name":"artifact_exists","passed":true}]"#,
                ),
            ),
            event("contract_pass", "contract passed", Some(r#"[]"#)),
        ];

        let metrics = extract_case_metrics(&events, &[], None);
        assert_eq!(metrics.contract_validation_attempt_count, 2);
        assert_eq!(metrics.contract_repair_retry_count, 1);
        assert_eq!(metrics.contract_violation_count, 2);
    }

    #[test]
    fn contract_violation_count_uses_exact_failure_count() {
        let metrics = extract_case_metrics(
            &[
                event("contract_fail", "no meta", None),
                event("contract_fail", "empty", Some(r#"[]"#)),
            ],
            &[],
            None,
        );
        assert_eq!(metrics.contract_validation_attempt_count, 2);
        assert_eq!(metrics.contract_repair_retry_count, 2);
        assert_eq!(metrics.contract_violation_count, 0);
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
