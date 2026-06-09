use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use serde_json::{Map, Value};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Instant;
use tokio::process::Command;
use tokio::time::{timeout, Duration};

use crate::agent::CustomToolHandler;
use crate::llm::ToolDefinition;
use crate::tools::ToolOutput;

use super::grader::GraderExecutionOutput;
use super::types::BenchmarkCase;

pub const SOP_SOURCE_SUITE: &str = "sop_bench";
pub const SOP_TASK_TYPE: &str = "sop_task_completion";
pub const SOP_GRADER_KIND: &str = "sop_bench";
pub const SOP_DATA_CSV: &str = "data.csv";

pub const SOP_DECISION_LABELS: &[&str] = &[
    "fulfill_immediately",
    "fulfill_delayed",
    "backorder",
    "reject",
    "manual_review",
];

const SOP_TOOL_NAMES: &[&str] = &[
    "check_inventory",
    "validate_customer",
    "calculate_shipping",
    "make_fulfillment_decision",
];

pub fn is_sop_case(case: &BenchmarkCase) -> bool {
    case.source_suite == SOP_SOURCE_SUITE || case.task_type == SOP_TASK_TYPE
}

pub fn row_map(columns: &[String], fields: &[String]) -> Map<String, Value> {
    columns
        .iter()
        .enumerate()
        .map(|(idx, column)| {
            (
                column.to_string(),
                Value::String(fields.get(idx).cloned().unwrap_or_default()),
            )
        })
        .collect()
}

pub fn build_sop_prompt(sop: &str, row: &Map<String, Value>) -> String {
    let mut fields = row
        .iter()
        .filter(|(key, _)| key.as_str() != "expected_output")
        .map(|(key, value)| format!("- {key}: {}", value.as_str().unwrap_or_default()))
        .collect::<Vec<_>>();
    fields.sort();
    format!(
        "Follow the SOP below for this one order. The four SOP tools are already available as native tools; do not read or inspect tools.py or toolspecs.json. You MUST call the tools in this exact order: check_inventory, validate_customer, calculate_shipping, then make_fulfillment_decision. Do not infer the decision manually; use the tool outputs. After Step 4, write the final LABEL into the blank expected_output cell in {SOP_DATA_CSV}, then call task_complete with summary exactly <final_decision>LABEL</final_decision> and no extra prose. Replace LABEL with exactly one of: {}\n\nSOP:\n{sop}\n\nOrder row fields:\n{}",
        fields.join("\n"),
        SOP_DECISION_LABELS.join(", ")
    )
}

pub fn sanitized_csv(columns: &[String], fields: &[String]) -> String {
    let sanitized = columns
        .iter()
        .enumerate()
        .map(|(idx, column)| {
            if column == "expected_output" {
                String::new()
            } else {
                fields.get(idx).cloned().unwrap_or_default()
            }
        })
        .collect::<Vec<_>>();
    format!(
        "{}\n{}\n",
        columns
            .iter()
            .map(|s| csv_escape(s))
            .collect::<Vec<_>>()
            .join(","),
        sanitized
            .iter()
            .map(|s| csv_escape(s))
            .collect::<Vec<_>>()
            .join(",")
    )
}

fn csv_escape(value: &str) -> String {
    if value.contains(',') || value.contains('"') || value.contains('\n') || value.contains('\r') {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

pub fn load_tool_definitions(workspace: &Path) -> Result<Vec<ToolDefinition>> {
    let specs_path = workspace.join("toolspecs.json");
    let specs: Value = serde_json::from_str(
        &fs::read_to_string(&specs_path)
            .with_context(|| format!("failed to read {}", specs_path.display()))?,
    )
    .with_context(|| format!("invalid JSON in {}", specs_path.display()))?;
    let specs = specs
        .as_array()
        .ok_or_else(|| anyhow!("SOP toolspecs must be an array"))?;
    let mut definitions = Vec::new();
    for spec in specs {
        let tool = spec
            .get("toolSpec")
            .ok_or_else(|| anyhow!("SOP toolspec entry missing toolSpec"))?;
        let name = tool
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("SOP toolSpec missing name"))?;
        if !SOP_TOOL_NAMES.contains(&name) {
            continue;
        }
        let description = tool
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("SOP-Bench tool")
            .to_string();
        let input_schema = tool
            .pointer("/inputSchema/json")
            .cloned()
            .ok_or_else(|| anyhow!("SOP toolSpec {name} missing inputSchema.json"))?;
        definitions.push(ToolDefinition {
            name: name.to_string(),
            description,
            input_schema,
            cache_control: None,
        });
    }
    if definitions.len() != SOP_TOOL_NAMES.len() {
        return Err(anyhow!(
            "SOP toolspecs provided {} known tools, expected {}",
            definitions.len(),
            SOP_TOOL_NAMES.len()
        ));
    }
    Ok(definitions)
}

pub struct SopBenchToolRuntime {
    workspace: PathBuf,
    definitions: Vec<ToolDefinition>,
    tool_names: BTreeSet<String>,
}

impl SopBenchToolRuntime {
    pub fn new(workspace: PathBuf) -> Result<Self> {
        let definitions = load_tool_definitions(&workspace)?;
        let tool_names = definitions
            .iter()
            .map(|definition| definition.name.clone())
            .collect::<BTreeSet<_>>();
        Ok(Self {
            workspace,
            definitions,
            tool_names,
        })
    }

    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.definitions.clone()
    }
}

#[async_trait]
impl CustomToolHandler for SopBenchToolRuntime {
    fn handles_tool(&self, name: &str) -> bool {
        self.tool_names.contains(name)
    }

    async fn execute_tool(&self, name: &str, input: &Value) -> ToolOutput {
        if !self.handles_tool(name) {
            return tool_error(
                "unknown_sop_tool",
                &format!("Unknown SOP-Bench tool: {name}"),
            );
        }
        let input_json = match serde_json::to_string(input) {
            Ok(value) => value,
            Err(err) => return tool_error("invalid_input", &err.to_string()),
        };
        let script = r#"
import contextlib
import io
import json
import sys
import types

if "pandas" not in sys.modules:
    sys.modules["pandas"] = types.ModuleType("pandas")

tool_name = sys.argv[1]
tool_input = json.loads(sys.argv[2])
with contextlib.redirect_stdout(io.StringIO()):
    from tools import OrderFulfillmentManager
    manager = OrderFulfillmentManager()
    result = manager.process_tool_call(tool_name, tool_input)
print(json.dumps(result, ensure_ascii=False))
"#;
        let mut cmd = Command::new("python3");
        cmd.arg("-c")
            .arg(script)
            .arg(name)
            .arg(input_json)
            .current_dir(&self.workspace)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let output = match timeout(Duration::from_secs(20), cmd.output()).await {
            Ok(Ok(output)) => output,
            Ok(Err(err)) => return tool_error("sop_tool_exec_failed", &err.to_string()),
            Err(_) => return tool_error("sop_tool_timeout", "SOP-Bench tool timed out after 20s"),
        };
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if !output.status.success() {
            return tool_error(
                "sop_tool_failed",
                &format!(
                    "SOP-Bench tool process exited with {:?}: {}",
                    output.status.code(),
                    stderr
                ),
            );
        }
        if serde_json::from_str::<Value>(&stdout).is_err() {
            return tool_error(
                "sop_tool_invalid_json",
                &format!("stdout was not JSON: {stdout}"),
            );
        }
        ToolOutput {
            content: stdout,
            is_error: false,
            meta: Some(serde_json::json!({ "kind": "sop_bench_tool", "tool": name })),
        }
    }
}

fn tool_error(kind: &str, message: &str) -> ToolOutput {
    ToolOutput {
        content: serde_json::json!({ "error": kind, "message": message }).to_string(),
        is_error: true,
        meta: Some(serde_json::json!({ "kind": "sop_bench_tool_error" })),
    }
}

pub fn grade_sop_response(
    case: &BenchmarkCase,
    response: &str,
    workspace: &Path,
) -> GraderExecutionOutput {
    let started = Instant::now();
    let expected = case.expected_output.as_deref().unwrap_or_default().trim();
    let tagged = extract_tagged_decision(response);
    let response_parsed = extract_sop_decision(response);
    let csv_parsed = extract_sop_decision_from_workspace_csv(workspace);
    let parsed = response_parsed.clone().or_else(|| csv_parsed.clone());
    let (actual, parse_strategy) = parsed
        .map(|parsed| (Some(parsed.label), parsed.strategy))
        .unwrap_or((None, "not_found".to_string()));
    let decision_success =
        actual.as_deref() == Some(expected) && SOP_DECISION_LABELS.contains(&expected);
    let format_success = tagged.as_deref() == Some(expected);
    let csv_success = csv_parsed
        .as_ref()
        .map(|parsed| parsed.label.as_str() == expected)
        .unwrap_or(false);
    let stdout_json = serde_json::json!({
        "task_success": decision_success,
        "decision_success": decision_success,
        "format_success": format_success,
        "csv_success": csv_success,
        "expected": expected,
        "actual": actual,
        "tagged_actual": tagged,
        "csv_actual": csv_parsed.as_ref().map(|parsed| parsed.label.as_str()),
        "parse_strategy": parse_strategy,
        "allowed_labels": SOP_DECISION_LABELS,
        "row_no": case.raw_json.get("row").and_then(Value::as_u64),
        "order_id": case.task_id,
        "raw_response_excerpt": response.chars().take(500).collect::<String>(),
    });
    GraderExecutionOutput {
        command: vec!["builtin:sop_bench".to_string()],
        exit_code: Some(0),
        stdout_json: Some(stdout_json),
        stderr: String::new(),
        duration_ms: started.elapsed().as_millis() as i64,
        task_success: Some(decision_success),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedDecision {
    label: String,
    strategy: String,
}

fn extract_sop_decision(response: &str) -> Option<ParsedDecision> {
    if let Some(label) = extract_tagged_decision(response) {
        return Some(ParsedDecision {
            label,
            strategy: "final_decision_tag".to_string(),
        });
    }
    if let Some(label) = extract_json_decision(response) {
        return Some(ParsedDecision {
            label,
            strategy: "json_decision".to_string(),
        });
    }
    let trimmed = response
        .trim()
        .trim_matches(|c: char| c == '.' || c == ',' || c == ';');
    if SOP_DECISION_LABELS.contains(&trimmed) {
        return Some(ParsedDecision {
            label: trimmed.to_string(),
            strategy: "exact_text".to_string(),
        });
    }
    let found = SOP_DECISION_LABELS
        .iter()
        .filter(|label| response.contains(**label))
        .collect::<Vec<_>>();
    if found.len() == 1 {
        return Some(ParsedDecision {
            label: (*found[0]).to_string(),
            strategy: "single_label_mention".to_string(),
        });
    }
    None
}

fn extract_tagged_decision(response: &str) -> Option<String> {
    let start_tag = "<final_decision>";
    let end_tag = "</final_decision>";
    let start = response.find(start_tag)? + start_tag.len();
    let end = response[start..].find(end_tag)? + start;
    let label = response[start..end].trim();
    SOP_DECISION_LABELS
        .contains(&label)
        .then(|| label.to_string())
}

fn extract_json_decision(response: &str) -> Option<String> {
    let value = serde_json::from_str::<Value>(response.trim()).ok()?;
    ["decision", "final_decision", "finalDecision"]
        .iter()
        .find_map(|key| {
            value
                .get(key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|label| SOP_DECISION_LABELS.contains(label))
                .map(str::to_string)
        })
}

fn extract_sop_decision_from_workspace_csv(workspace: &Path) -> Option<ParsedDecision> {
    let csv = fs::read_to_string(workspace.join(SOP_DATA_CSV)).ok()?;
    let line = csv.lines().nth(1)?;
    let label = line.rsplit(',').next()?.trim().trim_matches('"');
    SOP_DECISION_LABELS
        .contains(&label)
        .then(|| ParsedDecision {
            label: label.to_string(),
            strategy: "workspace_csv".to_string(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_prompt_without_expected_label() {
        let row = row_map(
            &[
                "order_id".to_string(),
                "product_id".to_string(),
                "expected_output".to_string(),
            ],
            &[
                "ORD001".to_string(),
                "PROD001".to_string(),
                "fulfill_immediately".to_string(),
            ],
        );
        let prompt = build_sop_prompt("SOP", &row);
        assert!(prompt.contains("ORD001"));
        assert!(prompt.contains("PROD001"));
        assert!(!prompt.contains("- expected_output:"));
    }

    #[test]
    fn sanitized_csv_blanks_expected_output() {
        let csv = sanitized_csv(
            &["order_id".to_string(), "expected_output".to_string()],
            &["ORD001".to_string(), "reject".to_string()],
        );
        assert_eq!(csv, "order_id,expected_output\nORD001,\n");
        assert!(!csv.contains("reject"));
    }

    fn test_case(expected: &str) -> BenchmarkCase {
        BenchmarkCase {
            id: "case-id".to_string(),
            suite_id: "suite-id".to_string(),
            task_id: "ORD001".to_string(),
            task_type: SOP_TASK_TYPE.to_string(),
            source_suite: SOP_SOURCE_SUITE.to_string(),
            target_tool_or_capability: "tool_based_sop_execution".to_string(),
            prompt: String::new(),
            assets: Vec::new(),
            expected_outputs: vec!["final_response".to_string()],
            grader: None,
            expected_output: Some(expected.to_string()),
            raw_json: serde_json::json!({ "row": 1 }),
            case_hash: "hash".to_string(),
            created_at: String::new(),
        }
    }

    #[test]
    fn grades_sop_response_against_expected_label() {
        let dir = tempfile::tempdir().unwrap();
        let output = grade_sop_response(
            &test_case("backorder"),
            "<final_decision>backorder</final_decision>",
            dir.path(),
        );
        assert_eq!(output.task_success, Some(true));
        let stdout = output.stdout_json.unwrap();
        assert_eq!(stdout["actual"], "backorder");
        assert_eq!(stdout["parse_strategy"], "final_decision_tag");
        assert_eq!(stdout["decision_success"], true);
        assert_eq!(stdout["format_success"], true);

        let output = grade_sop_response(&test_case("reject"), "backorder", dir.path());
        assert_eq!(output.task_success, Some(false));
    }

    #[test]
    fn separates_decision_success_from_format_success() {
        let dir = tempfile::tempdir().unwrap();
        let output = grade_sop_response(
            &test_case("manual_review"),
            "Final decision: manual_review",
            dir.path(),
        );
        assert_eq!(output.task_success, Some(true));
        let stdout = output.stdout_json.unwrap();
        assert_eq!(stdout["decision_success"], true);
        assert_eq!(stdout["format_success"], false);
        assert_eq!(stdout["parse_strategy"], "single_label_mention");
    }
    #[test]
    fn default_coding_tools_do_not_include_sop_tools() {
        let default_tool_names = crate::tools::coding_agent_tools_with_artifact_support()
            .into_iter()
            .map(|tool| tool.name)
            .collect::<BTreeSet<_>>();
        for sop_tool in SOP_TOOL_NAMES {
            assert!(!default_tool_names.contains(*sop_tool));
        }
    }

    #[tokio::test]
    async fn sop_tool_runtime_executes_workspace_tools_py() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("toolspecs.json"),
            r#"[
              {"toolSpec":{"name":"check_inventory","description":"Check inventory","inputSchema":{"json":{"type":"object","properties":{"product_id":{"type":"string"},"quantity_requested":{"type":"integer"}},"required":["product_id","quantity_requested"]}}}},
              {"toolSpec":{"name":"validate_customer","description":"Validate customer","inputSchema":{"json":{"type":"object","properties":{"customer_id":{"type":"string"},"order_total":{"type":"number"}},"required":["customer_id","order_total"]}}}},
              {"toolSpec":{"name":"calculate_shipping","description":"Calculate shipping","inputSchema":{"json":{"type":"object","properties":{"destination_zip":{"type":"string"},"package_weight":{"type":"number"},"shipping_speed":{"type":"string"}},"required":["destination_zip","package_weight","shipping_speed"]}}}},
              {"toolSpec":{"name":"make_fulfillment_decision","description":"Decide","inputSchema":{"json":{"type":"object","properties":{"inventory_status":{"type":"string"},"customer_status":{"type":"string"},"shipping_cost":{"type":"number"},"order_priority":{"type":"string"}},"required":["inventory_status","customer_status","shipping_cost","order_priority"]}}}}
            ]"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("tools.py"),
            r#"
import json
import pandas as pd
class OrderFulfillmentManager:
    def __init__(self):
        print("init noise")
    def process_tool_call(self, tool_name, tool_input):
        if tool_name == "check_inventory":
            return {"inventory_status": "in_stock", "available_quantity": 10}
        raise ValueError(f"Invalid tool_name: {tool_name}")
"#,
        )
        .unwrap();
        let runtime = SopBenchToolRuntime::new(dir.path().to_path_buf()).unwrap();
        assert_eq!(runtime.definitions().len(), 4);
        let output = runtime
            .execute_tool(
                "check_inventory",
                &serde_json::json!({"product_id": "PROD001", "quantity_requested": 2}),
            )
            .await;
        assert!(!output.is_error, "{}", output.content);
        assert_eq!(
            serde_json::from_str::<Value>(&output.content).unwrap()["inventory_status"],
            "in_stock"
        );
    }
}
