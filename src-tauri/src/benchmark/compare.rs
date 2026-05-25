use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::types::{BenchmarkMetrics, BenchmarkResult, BenchmarkRun};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BenchmarkRunComparison {
    pub baseline_run_id: String,
    pub candidate_run_id: String,
    pub common_case_count: usize,
    pub baseline_only_case_ids: Vec<String>,
    pub candidate_only_case_ids: Vec<String>,
    pub aggregate_delta: MetricDelta,
    pub per_case: Vec<CaseDelta>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MetricDelta {
    pub success_delta: Option<f64>,
    pub total_tokens_delta: i64,
    pub tool_call_count_delta: i64,
    pub llm_request_count_delta: i64,
    pub runtime_ms_delta: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaseDelta {
    pub case_id: String,
    pub baseline_success: Option<bool>,
    pub candidate_success: Option<bool>,
    pub classification: String,
    pub total_tokens_delta: Option<i64>,
    pub tool_call_count_delta: Option<i64>,
    pub llm_request_count_delta: Option<i64>,
    pub runtime_ms_delta: Option<i64>,
}

pub fn compare_runs(
    baseline: &BenchmarkRun,
    candidate: &BenchmarkRun,
    baseline_results: &[BenchmarkResult],
    candidate_results: &[BenchmarkResult],
    baseline_metrics: &HashMap<String, BenchmarkMetrics>,
    candidate_metrics: &HashMap<String, BenchmarkMetrics>,
) -> BenchmarkRunComparison {
    let b_by_case: HashMap<&str, &BenchmarkResult> = baseline_results
        .iter()
        .map(|r| (r.case_id.as_str(), r))
        .collect();
    let c_by_case: HashMap<&str, &BenchmarkResult> = candidate_results
        .iter()
        .map(|r| (r.case_id.as_str(), r))
        .collect();

    let mut common = Vec::new();
    let mut baseline_only = Vec::new();
    for case_id in b_by_case.keys() {
        if c_by_case.contains_key(case_id) {
            common.push((*case_id).to_string());
        } else {
            baseline_only.push((*case_id).to_string());
        }
    }
    let mut candidate_only = Vec::new();
    for case_id in c_by_case.keys() {
        if !b_by_case.contains_key(case_id) {
            candidate_only.push((*case_id).to_string());
        }
    }
    common.sort();
    baseline_only.sort();
    candidate_only.sort();

    let mut per_case = Vec::with_capacity(common.len());
    let mut aggregate_delta = MetricDelta::default();
    let mut baseline_successes = 0;
    let mut candidate_successes = 0;

    for case_id in &common {
        let b = b_by_case[case_id.as_str()];
        let c = c_by_case[case_id.as_str()];
        if b.success == Some(true) {
            baseline_successes += 1;
        }
        if c.success == Some(true) {
            candidate_successes += 1;
        }
        let bm = baseline_metrics.get(case_id);
        let cm = candidate_metrics.get(case_id);
        let total_tokens_delta = delta_i64(bm.map(|m| m.total_tokens), cm.map(|m| m.total_tokens));
        let tool_call_count_delta =
            delta_i64(bm.map(|m| m.tool_call_count), cm.map(|m| m.tool_call_count));
        let llm_request_count_delta = delta_i64(
            bm.map(|m| m.llm_request_count),
            cm.map(|m| m.llm_request_count),
        );
        let runtime_ms_delta =
            delta_i64(bm.and_then(|m| m.runtime_ms), cm.and_then(|m| m.runtime_ms));

        aggregate_delta.total_tokens_delta += total_tokens_delta.unwrap_or(0);
        aggregate_delta.tool_call_count_delta += tool_call_count_delta.unwrap_or(0);
        aggregate_delta.llm_request_count_delta += llm_request_count_delta.unwrap_or(0);
        aggregate_delta.runtime_ms_delta =
            Some(aggregate_delta.runtime_ms_delta.unwrap_or(0) + runtime_ms_delta.unwrap_or(0));

        per_case.push(CaseDelta {
            case_id: case_id.clone(),
            baseline_success: b.success,
            candidate_success: c.success,
            classification: classify(b.success, c.success).to_string(),
            total_tokens_delta,
            tool_call_count_delta,
            llm_request_count_delta,
            runtime_ms_delta,
        });
    }

    aggregate_delta.success_delta = if common.is_empty() {
        None
    } else {
        Some((candidate_successes - baseline_successes) as f64 / common.len() as f64)
    };

    BenchmarkRunComparison {
        baseline_run_id: baseline.id.clone(),
        candidate_run_id: candidate.id.clone(),
        common_case_count: common.len(),
        baseline_only_case_ids: baseline_only,
        candidate_only_case_ids: candidate_only,
        aggregate_delta,
        per_case,
    }
}

fn delta_i64(baseline: Option<i64>, candidate: Option<i64>) -> Option<i64> {
    Some(candidate? - baseline?)
}

fn classify(baseline: Option<bool>, candidate: Option<bool>) -> &'static str {
    match (baseline, candidate) {
        (Some(false), Some(true)) | (None, Some(true)) => "improvement",
        (Some(true), Some(false)) | (Some(true), None) => "regression",
        _ => "unchanged",
    }
}
