use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BenchmarkSourceKind {
    GaToolEfficiency,
    GaSopBench,
    GaLifelongAgentbench,
    GaRealfinBenchmark,
    Custom,
}

impl BenchmarkSourceKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::GaToolEfficiency => "ga_tool_efficiency",
            Self::GaSopBench => "ga_sop_bench",
            Self::GaLifelongAgentbench => "ga_lifelong_agentbench",
            Self::GaRealfinBenchmark => "ga_realfin_benchmark",
            Self::Custom => "custom",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BenchmarkAgentKind {
    Coding,
    Planner,
    Evaluator,
    Chat,
}

impl BenchmarkAgentKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Coding => "coding",
            Self::Planner => "planner",
            Self::Evaluator => "evaluator",
            Self::Chat => "chat",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BenchmarkSuite {
    pub id: String,
    pub name: String,
    pub description: String,
    pub source_kind: String,
    pub source_path: String,
    pub source_ref: Option<String>,
    pub manifest_json: serde_json::Value,
    pub case_count: i64,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BenchmarkCase {
    pub id: String,
    pub suite_id: String,
    pub task_id: String,
    pub task_type: String,
    pub source_suite: String,
    pub target_tool_or_capability: String,
    pub prompt: String,
    pub assets: Vec<String>,
    pub expected_outputs: Vec<String>,
    pub grader: Option<BenchmarkGraderSpec>,
    pub expected_output: Option<String>,
    pub raw_json: serde_json::Value,
    pub case_hash: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BenchmarkGraderSpec {
    pub kind: String,
    pub path: String,
}

impl BenchmarkGraderSpec {
    pub fn python(path: impl Into<String>) -> Self {
        Self {
            kind: "python".to_string(),
            path: path.into(),
        }
    }

    pub fn sop_bench() -> Self {
        Self {
            kind: crate::benchmark::sop_bench::SOP_GRADER_KIND.to_string(),
            path: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BenchmarkRun {
    pub id: String,
    pub suite_id: String,
    pub name: String,
    pub status: String,
    pub agent_kind: String,
    pub provider: String,
    pub model: String,
    pub base_url_hash: Option<String>,
    pub agent_config_json: serde_json::Value,
    pub git_commit: Option<String>,
    pub git_dirty: bool,
    pub benchmark_source_path: String,
    pub case_ids: Vec<String>,
    pub timeout_seconds: Option<i64>,
    pub max_steps: Option<i64>,
    pub token_budget: Option<i64>,
    pub cost_budget_usd: Option<f64>,
    pub workspace_root: Option<String>,
    pub metadata_json: serde_json::Value,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BenchmarkResult {
    pub id: String,
    pub run_id: String,
    pub case_id: String,
    pub agent_id: Option<String>,
    pub workspace_path: Option<String>,
    pub status: String,
    pub success: Option<bool>,
    pub grading_status: String,
    pub final_response: Option<String>,
    pub artifact_refs: Vec<String>,
    pub error_message: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct BenchmarkMetrics {
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_tokens: i64,
    pub cost_usd: f64,
    pub llm_request_count: i64,
    pub tool_call_count: i64,
    pub tool_result_count: i64,
    pub tool_error_count: i64,
    pub tool_call_count_by_name: BTreeMap<String, i64>,
    pub runtime_ms: Option<i64>,
    pub successful_case_count: Option<i64>,
    pub graded_case_count: Option<i64>,
    pub total_case_count: Option<i64>,
    pub all_cases_tsr: Option<f64>,
    pub graded_cases_tsr: Option<f64>,
    pub token_per_success: Option<f64>,
    pub tool_calls_per_success: Option<f64>,
    pub requests_per_success: Option<f64>,
    pub tool_error_rate: Option<f64>,
    pub guardrail_retry_count: i64,
    pub recovery_attempt_count: i64,
    pub read_only_loop_hint_count: i64,
    pub context_saved_chars: i64,
    pub tool_result_ref_count: i64,
    pub tool_result_repeat_count: i64,
    pub evidence_read_ref_count: i64,
    pub shell_content_command_count: i64,
    pub persisted_tool_result_count: i64,
    pub per_message_budget_replacement_count: i64,
    pub contract_validation_attempt_count: i64,
    pub contract_violation_count: i64,
    pub contract_repair_retry_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BenchmarkMetricSnapshot {
    pub id: String,
    pub run_id: String,
    pub result_id: Option<String>,
    pub scope: String,
    pub metrics: BenchmarkMetrics,
    pub raw_json: serde_json::Value,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraderArtifact {
    pub id: String,
    pub result_id: String,
    pub grader_kind: String,
    pub command: Vec<String>,
    pub exit_code: Option<i32>,
    pub stdout_json: Option<serde_json::Value>,
    pub stderr: String,
    pub duration_ms: i64,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BenchmarkSummary {
    pub run: BenchmarkRun,
    pub suite: BenchmarkSuite,
    pub metrics: Option<BenchmarkMetricSnapshot>,
    pub results: Vec<BenchmarkResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportBenchmarkSuiteOutput {
    pub suite: BenchmarkSuite,
    pub cases: Vec<BenchmarkCase>,
}

#[derive(Debug, Clone)]
pub struct ImportedSuiteDraft {
    pub suite_id: String,
    pub name: String,
    pub description: String,
    pub source_kind: BenchmarkSourceKind,
    pub source_path: String,
    pub source_ref: Option<String>,
    pub manifest_json: serde_json::Value,
    pub cases: Vec<ImportedCaseDraft>,
}

#[derive(Debug, Clone)]
pub struct ImportedCaseDraft {
    pub id: String,
    pub task_id: String,
    pub task_type: String,
    pub source_suite: String,
    pub target_tool_or_capability: String,
    pub prompt: String,
    pub assets: Vec<String>,
    pub expected_outputs: Vec<String>,
    pub grader: Option<BenchmarkGraderSpec>,
    pub expected_output: Option<String>,
    pub raw_json: serde_json::Value,
    pub case_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BenchmarkRunConfig {
    pub name: Option<String>,
    pub case_ids: Option<Vec<String>>,
    pub timeout_seconds: Option<i64>,
    pub max_steps: Option<i64>,
    pub token_budget: Option<i64>,
    pub cost_budget_usd: Option<f64>,
    pub workspace_root: Option<String>,
    pub trace_root: Option<String>,
}
