use anyhow::Result;

use super::types::BenchmarkSummary;

pub fn export_summary_json(summary: &BenchmarkSummary) -> Result<String> {
    Ok(serde_json::to_string_pretty(summary)?)
}

pub fn export_summary_markdown(summary: &BenchmarkSummary) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Benchmark Run: {}\n\n", summary.run.name));
    out.push_str("## Configuration\n\n");
    out.push_str(&format!("- Suite: {}\n", summary.suite.name));
    out.push_str(&format!("- Provider: {}\n", summary.run.provider));
    out.push_str(&format!("- Model: {}\n", summary.run.model));
    out.push_str(&format!("- Agent kind: {}\n", summary.run.agent_kind));
    out.push_str(&format!("- Status: {}\n\n", summary.run.status));

    if let Some(snapshot) = &summary.metrics {
        let m = &snapshot.metrics;
        out.push_str("## Metrics\n\n");
        out.push_str("| Metric | Value |\n|---|---:|\n");
        out.push_str(&format!(
            "| Total cases | {} |\n",
            m.total_case_count.unwrap_or(0)
        ));
        out.push_str(&format!(
            "| Successful cases | {} |\n",
            m.successful_case_count.unwrap_or(0)
        ));
        out.push_str(&format!(
            "| All-cases TSR | {} |\n",
            fmt_opt(m.all_cases_tsr)
        ));
        out.push_str(&format!(
            "| Graded-cases TSR | {} |\n",
            fmt_opt(m.graded_cases_tsr)
        ));
        out.push_str(&format!("| Total tokens | {} |\n", m.total_tokens));
        out.push_str(&format!("| Cost USD | {:.6} |\n", m.cost_usd));
        out.push_str(&format!("| LLM requests | {} |\n", m.llm_request_count));
        out.push_str(&format!("| Tool calls | {} |\n", m.tool_call_count));
        out.push_str(&format!("| Tool errors | {} |\n", m.tool_error_count));
        out.push_str(&format!(
            "| Context saved chars | {} |\n",
            m.context_saved_chars
        ));
        out.push_str(&format!(
            "| Tool result refs | {} |\n",
            m.tool_result_ref_count
        ));
        out.push_str(&format!(
            "| Tool result repeats | {} |\n",
            m.tool_result_repeat_count
        ));
        out.push_str(&format!(
            "| Evidence read refs | {} |\n",
            m.evidence_read_ref_count
        ));
        out.push_str(&format!(
            "| Shell content commands | {} |\n",
            m.shell_content_command_count
        ));
        out.push_str(&format!(
            "| Persisted tool results | {} |\n",
            m.persisted_tool_result_count
        ));
        out.push_str(&format!(
            "| Per-message budget replacements | {} |\n",
            m.per_message_budget_replacement_count
        ));
        out.push_str(&format!(
            "| Contract validation attempts | {} |\n",
            m.contract_validation_attempt_count
        ));
        out.push_str(&format!(
            "| Contract violations | {} |\n",
            m.contract_violation_count
        ));
        out.push_str(&format!(
            "| Contract repair retries | {} |\n\n",
            m.contract_repair_retry_count
        ));
    }

    out.push_str("## Cases\n\n");
    out.push_str(
        "| Case | Status | Success | Grading | Tokens/Artifacts |\n|---|---|---|---|---:|\n",
    );
    for result in &summary.results {
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} |\n",
            result.case_id,
            result.status,
            result
                .success
                .map(|s| s.to_string())
                .unwrap_or_else(|| "ungraded".to_string()),
            result.grading_status,
            result.artifact_refs.len()
        ));
    }
    out
}

pub fn export_summary_csv(summary: &BenchmarkSummary) -> String {
    let mut out =
        String::from("result_id,case_id,status,success,grading_status,agent_id,workspace_path\n");
    for result in &summary.results {
        out.push_str(&format!(
            "{},{},{},{},{},{},{}\n",
            csv(&result.id),
            csv(&result.case_id),
            csv(&result.status),
            csv(&result.success.map(|s| s.to_string()).unwrap_or_default()),
            csv(&result.grading_status),
            csv(result.agent_id.as_deref().unwrap_or_default()),
            csv(result.workspace_path.as_deref().unwrap_or_default())
        ));
    }
    out
}

fn fmt_opt(value: Option<f64>) -> String {
    value
        .map(|v| format!("{:.4}", v))
        .unwrap_or_else(|| "n/a".to_string())
}

fn csv(value: &str) -> String {
    if value.contains(',') || value.contains('"') || value.contains('\n') {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}
