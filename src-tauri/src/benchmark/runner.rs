use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;
use tauri::Manager;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::agent::guardrail::{Guardrail, SummaryMatchMode};
use crate::agent::{AgentEngine, AgentRunOptions, AgentStatus};
use crate::commands::ConfigManager;
use crate::db::{queries, Database};
use crate::llm::{AnthropicProvider, LlmProvider, OpenAICompatProvider};

use super::grader::execute_python_grader;
use super::importer::{import_suite_from_path, resolve_asset_paths};
use super::metrics::{aggregate_run_metrics, extract_case_metrics, CostRecordInput};
use super::types::{
    BenchmarkCase, BenchmarkMetricSnapshot, BenchmarkMetrics, BenchmarkResult, BenchmarkRun,
    BenchmarkRunConfig, BenchmarkSuite, BenchmarkSummary,
};

pub fn prepare_case_workspace(
    source_root: &Path,
    case: &BenchmarkCase,
    workspace_root: &Path,
) -> Result<PathBuf> {
    let workspace = workspace_root.join(&case.task_id);
    if workspace.exists() {
        fs::remove_dir_all(&workspace)
            .with_context(|| format!("failed to reset workspace {}", workspace.display()))?;
    }
    fs::create_dir_all(&workspace)
        .with_context(|| format!("failed to create workspace {}", workspace.display()))?;

    for asset in resolve_asset_paths(&case.assets, source_root) {
        copy_asset(source_root, &asset, &workspace, &case.task_id)?;
    }
    Ok(workspace)
}

pub async fn import_and_run_benchmark(
    app: tauri::AppHandle,
    suite_path: PathBuf,
    config: BenchmarkRunConfig,
) -> Result<BenchmarkSummary> {
    let db = app.state::<Database>();
    let app_config = app.state::<ConfigManager>().get_config_snapshot();
    let imported = import_suite_from_path(&suite_path)?;
    let suite_id = imported.suite_id.clone();
    db.with_conn(|conn| queries::upsert_benchmark_suite_with_cases(conn, &imported))?;

    let all_cases = db.with_conn(|conn| queries::list_benchmark_cases(conn, &suite_id))?;
    let selected_cases = select_benchmark_cases(&all_cases, config.case_ids.as_deref())?;
    let selected_case_ids = selected_cases
        .iter()
        .map(|case| case.id.clone())
        .collect::<Vec<_>>();
    if selected_case_ids.is_empty() {
        return Err(anyhow!("benchmark run has no selected cases"));
    }

    let run_id = Uuid::new_v4().to_string();
    let workspace_root = config
        .workspace_root
        .clone()
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::temp_dir()
                .join("miragenty-benchmark")
                .join(&run_id)
        });
    fs::create_dir_all(&workspace_root).with_context(|| {
        format!(
            "failed to create workspace root {}",
            workspace_root.display()
        )
    })?;
    let trace_root = config.trace_root.clone().map(PathBuf::from);

    let run_name = config.name.clone().unwrap_or_else(|| {
        format!(
            "{} {}",
            imported.name,
            chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC")
        )
    });
    let case_ids_json = serde_json::to_string(&selected_case_ids)?;
    let provider_name = app_config.provider.clone();
    let model_name = app_config.default_model.clone();
    let agent_config_json = serde_json::to_string(&serde_json::json!({
        "provider": provider_name,
        "model": model_name,
        "maxAgentSteps": app_config.max_agent_steps,
        "agentTimeoutSeconds": app_config.agent_timeout_seconds,
        "agentMaxOutputTokens": app_config.agent_max_output_tokens,
        "agentOutputTokenBudget": app_config.agent_output_token_budget,
        "fallbackModel": app_config.agent_fallback_model,
        "fallbackSticky": app_config.agent_fallback_sticky,
    }))?;
    db.with_conn(|conn| {
        queries::insert_benchmark_run(
            conn,
            &run_id,
            &suite_id,
            &run_name,
            "coding",
            &provider_name,
            &model_name,
            None,
            &agent_config_json,
            current_git_commit().as_deref(),
            current_git_dirty(),
            &imported.source_path,
            &case_ids_json,
            config.timeout_seconds,
            config.max_steps,
            config.token_budget,
            config.cost_budget_usd,
            Some(&workspace_root.to_string_lossy()),
            &serde_json::to_string(&serde_json::json!({
                "entrypoint": "dev_cli",
                "suitePath": suite_path.to_string_lossy(),
                "traceRoot": trace_root.as_ref().map(|path| path.to_string_lossy().to_string()),
            }))?,
        )?;
        queries::update_benchmark_run_status(conn, &run_id, "running")
    })?;

    println!(
        "Benchmark run started: run_id={} suite=\"{}\" cases={} model={} trace_root={}",
        run_id,
        imported.name,
        selected_case_ids.len(),
        model_name,
        trace_root
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "disabled".to_string())
    );

    let mut final_status = "completed";
    let total_cases = selected_case_ids.len();
    for (idx, case_id) in selected_case_ids.iter().enumerate() {
        let case_no = idx + 1;
        let case_started = Instant::now();
        let Some(case) = db.with_conn(|conn| queries::get_benchmark_case(conn, case_id))? else {
            final_status = "completed_with_failures";
            println!("[{case_no}/{total_cases}] missing case id={case_id}");
            continue;
        };
        let run = load_benchmark_run(&db, &run_id)?;
        let case = benchmark_case_from_row(case)?;
        println!("[{case_no}/{total_cases}] {} started", case.task_id);
        match run_single_case(
            app.clone(),
            run,
            case.clone(),
            PathBuf::from(&imported.source_path),
            workspace_root.clone(),
            trace_root.clone(),
        )
        .await
        {
            Ok(progress) => {
                println!(
                    "[{case_no}/{total_cases}] {} finished status={} success={} grading={} tokens={} tools={} errors={} cost_usd={:.6} elapsed={}s trace={}",
                    progress.task_id,
                    progress.status,
                    progress
                        .success
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "ungraded".to_string()),
                    progress.grading_status,
                    progress.total_tokens,
                    progress.tool_calls,
                    progress.tool_errors,
                    progress.cost_usd,
                    case_started.elapsed().as_secs(),
                    progress
                        .trace_path
                        .as_ref()
                        .map(|path| path.display().to_string())
                        .unwrap_or_else(|| "n/a".to_string())
                );
                if progress.status != "completed" {
                    final_status = "completed_with_failures";
                }
            }
            Err(err) => {
                final_status = "completed_with_failures";
                println!(
                    "[{case_no}/{total_cases}] {} failed before result status could be recorded: {} elapsed={}s",
                    case.task_id,
                    err,
                    case_started.elapsed().as_secs()
                );
                tracing::warn!(case_id = %case_id, error = %err, "benchmark case failed");
            }
        }
    }

    let results = db.with_conn(|conn| queries::list_benchmark_results(conn, &run_id))?;
    if results.iter().any(|r| r.status != "completed") {
        final_status = "completed_with_failures";
    }
    let case_metrics = db.with_conn(|conn| queries::list_case_metric_snapshots(conn, &run_id))?;
    let result_models = results
        .into_iter()
        .map(benchmark_result_from_row)
        .collect::<Result<Vec<_>>>()?;
    let metric_models = case_metrics
        .into_iter()
        .map(benchmark_metric_from_row)
        .collect::<Result<Vec<_>>>()?;
    let aggregate = aggregate_run_metrics(
        &metric_models
            .iter()
            .map(|m| m.metrics.clone())
            .collect::<Vec<_>>(),
        &result_models,
    );
    db.with_conn(|conn| {
        queries::insert_benchmark_metric_snapshot(
            conn,
            &Uuid::new_v4().to_string(),
            &run_id,
            None,
            "run",
            &aggregate,
            &serde_json::json!({}),
        )?;
        queries::update_benchmark_run_status(conn, &run_id, final_status)
    })?;

    load_benchmark_summary(&db, &run_id)
}

pub fn load_benchmark_summary(db: &Database, run_id: &str) -> Result<BenchmarkSummary> {
    let run = load_benchmark_run(db, run_id)?;
    let suite = db
        .with_conn(|conn| queries::get_benchmark_suite(conn, &run.suite_id))?
        .ok_or_else(|| anyhow!("benchmark suite not found: {}", run.suite_id))?;
    let metrics = db
        .with_conn(|conn| queries::latest_benchmark_metric_snapshot(conn, run_id, "run"))?
        .map(benchmark_metric_from_row)
        .transpose()?;
    let results = db
        .with_conn(|conn| queries::list_benchmark_results(conn, run_id))?
        .into_iter()
        .map(benchmark_result_from_row)
        .collect::<Result<Vec<_>>>()?;
    Ok(BenchmarkSummary {
        run,
        suite: benchmark_suite_from_row(suite)?,
        metrics,
        results,
    })
}

pub fn load_benchmark_run(db: &Database, run_id: &str) -> Result<BenchmarkRun> {
    db.with_conn(|conn| queries::get_benchmark_run(conn, run_id))?
        .ok_or_else(|| anyhow!("benchmark run not found: {run_id}"))
        .and_then(benchmark_run_from_row)
}

pub struct CaseProgress {
    task_id: String,
    status: String,
    success: Option<bool>,
    grading_status: String,
    total_tokens: i64,
    tool_calls: i64,
    tool_errors: i64,
    cost_usd: f64,
    trace_path: Option<PathBuf>,
}
pub async fn run_single_case(
    app: tauri::AppHandle,
    run: BenchmarkRun,
    case: BenchmarkCase,
    source_root: PathBuf,
    workspace_root: PathBuf,
    trace_root: Option<PathBuf>,
) -> Result<CaseProgress> {
    let db = app.state::<Database>();
    let config_mgr = app.state::<ConfigManager>();
    let config = config_mgr.get_config_snapshot();
    let api_key = config_mgr
        .get_api_key(&config.provider)
        .or_else(|| config_mgr.get_api_key("default"))
        .ok_or_else(|| anyhow!("missing API key for provider {}", config.provider))?;
    let provider: Arc<dyn LlmProvider> = match config.provider.as_str() {
        "anthropic" => Arc::new(AnthropicProvider::with_stream_idle(
            api_key,
            config.agent_step_idle_seconds,
        )),
        _ => Arc::new(OpenAICompatProvider::with_stream_idle(
            api_key,
            config.base_url.clone(),
            config.agent_step_idle_seconds,
        )),
    };

    let workspace = prepare_case_workspace(&source_root, &case, &workspace_root)?;
    let agent_id = Uuid::new_v4().to_string();
    let result_id = Uuid::new_v4().to_string();
    db.with_conn(|conn| {
        queries::insert_benchmark_agent(
            conn,
            &agent_id,
            &case.task_id,
            &workspace.to_string_lossy(),
        )?;
        queries::insert_benchmark_result(
            conn,
            &result_id,
            &run.id,
            &case.id,
            Some(&agent_id),
            Some(&workspace.to_string_lossy()),
        )?;
        queries::mark_benchmark_result_running(conn, &result_id)?;
        Ok(())
    })?;

    let cancel_token = CancellationToken::new();
    let engine = AgentEngine::new(provider, workspace.clone(), app.clone(), cancel_token);
    let opts = AgentRunOptions {
        model: run.model.clone(),
        max_steps: run
            .max_steps
            .unwrap_or(config.max_agent_steps as i64)
            .max(1) as u32,
        timeout_secs: run
            .timeout_seconds
            .unwrap_or(config.agent_timeout_seconds as i64)
            .max(1) as u64,
        max_output_tokens: config.agent_max_output_tokens,
        stream_network_retries: config.stream_network_retries,
        stream_initial_retry_delay_ms: config.stream_initial_retry_delay_ms,
        output_token_budget: run
            .token_budget
            .or_else(|| {
                (config.agent_output_token_budget > 0)
                    .then_some(config.agent_output_token_budget as i64)
            })
            .map(|v| v as u64),
        fallback_model: (!config.agent_fallback_model.trim().is_empty())
            .then_some(config.agent_fallback_model.clone()),
        fallback_sticky: config.agent_fallback_sticky,
        guardrails: benchmark_guardrails(&case),
        guardrail_retry_budget: 2,
        ..AgentRunOptions::default()
    };

    let task_prompt = benchmark_task_prompt(&case.prompt);
    let (status, run_error) = match engine
        .run_with_options(&agent_id, &task_prompt, &opts)
        .await
    {
        Ok(status) => (status, None),
        Err(err) => (AgentStatus::Failed, Some(err.to_string())),
    };
    let final_response = latest_final_response(&db, &agent_id)?;
    let response_file = workspace.join("final_response.txt");
    fs::write(
        &response_file,
        final_response.as_deref().unwrap_or_default(),
    )?;

    let mut success = None;
    let mut grading_status = "ungraded".to_string();
    if let Some(grader) = &case.grader {
        match execute_python_grader(&source_root, grader, &workspace, &response_file, 120).await {
            Ok(output) => {
                success = output.task_success;
                grading_status = if output.exit_code == Some(0) {
                    "passed"
                } else {
                    "failed"
                }
                .to_string();
                let artifact_id = Uuid::new_v4().to_string();
                db.with_conn(|conn| {
                    queries::insert_benchmark_grader_artifact(
                        conn,
                        &artifact_id,
                        &result_id,
                        "python",
                        &serde_json::to_string(&output.command)?,
                        output.exit_code,
                        output
                            .stdout_json
                            .as_ref()
                            .map(serde_json::to_string)
                            .transpose()?
                            .as_deref(),
                        &output.stderr,
                        output.duration_ms,
                    )?;
                    Ok(())
                })?;
            }
            Err(err) => {
                grading_status = "failed".to_string();
                db.with_conn(|conn| {
                    queries::insert_benchmark_grader_artifact(
                        conn,
                        &Uuid::new_v4().to_string(),
                        &result_id,
                        "python",
                        "[]",
                        None,
                        None,
                        &err.to_string(),
                        0,
                    )?;
                    Ok(())
                })?;
            }
        }
    }

    let result_status = match status {
        AgentStatus::Completed => "completed",
        AgentStatus::Cancelled => "cancelled",
        AgentStatus::Failed => "failed",
        _ => "failed",
    };
    let metrics = db.with_conn(|conn| {
        queries::complete_benchmark_result(
            conn,
            &result_id,
            result_status,
            success,
            &grading_status,
            final_response.as_deref(),
            run_error.as_deref(),
        )?;
        let events = queries::get_events_for_agent(conn, &agent_id)?;
        let costs = queries::list_cost_records_for_agent(conn, &agent_id)?
            .into_iter()
            .map(|c| CostRecordInput {
                input_tokens: c.input_tokens,
                output_tokens: c.output_tokens,
                cost_usd: c.cost_usd,
            })
            .collect::<Vec<_>>();
        let metrics = extract_case_metrics(&events, &costs, None);
        queries::insert_benchmark_metric_snapshot(
            conn,
            &Uuid::new_v4().to_string(),
            &run.id,
            Some(&result_id),
            "case",
            &metrics,
            &serde_json::json!({}),
        )?;
        Ok(metrics)
    })?;
    let trace_path = if let Some(root) = trace_root {
        export_case_trace(&db, &run, &case, &result_id, &root)?;
        Some(root.join(&run.id).join(safe_path_segment(&case.task_id)))
    } else {
        None
    };
    Ok(CaseProgress {
        task_id: case.task_id,
        status: result_status.to_string(),
        success,
        grading_status,
        total_tokens: metrics.total_tokens,
        tool_calls: metrics.tool_call_count,
        tool_errors: metrics.tool_error_count,
        cost_usd: metrics.cost_usd,
        trace_path,
    })
}

fn benchmark_task_prompt(prompt: &str) -> String {
    format!(
        "{prompt}\n\n[Benchmark harness instruction]\nWhen you finish, call task_complete exactly once. The task_complete summary must contain the exact final answer requested by the task, not a meta-summary of what you did. If the task asks for a JSON/text/code block, put that block verbatim in task_complete.summary with no extra explanation."
    )
}

fn benchmark_guardrails(case: &BenchmarkCase) -> Vec<Guardrail> {
    let mut guardrails = Vec::new();
    if case.expected_outputs.iter().any(|output| output == "final_response") {
        if case.prompt.contains("```json") {
            guardrails.push(Guardrail::SummaryMatches {
                mode: SummaryMatchMode::JsonCodeBlock,
            });
        } else if case.prompt.contains("```text") {
            guardrails.push(Guardrail::SummaryMatches {
                mode: SummaryMatchMode::TextCodeBlock,
            });
        } else if case.prompt.contains("输出 `OK`") || case.prompt.contains("输出 `OK` 即可") {
            guardrails.push(Guardrail::SummaryMatches {
                mode: SummaryMatchMode::ExactOk,
            });
        }
    }
    guardrails
}

fn select_benchmark_cases(
    all_cases: &[queries::BenchmarkCaseRow],
    requested: Option<&[String]>,
) -> Result<Vec<queries::BenchmarkCaseRow>> {
    let Some(requested) = requested else {
        return Ok(all_cases.to_vec());
    };
    let mut selected = Vec::new();
    for requested_id in requested {
        let Some(case) = all_cases
            .iter()
            .find(|case| case.id == *requested_id || case.task_id == *requested_id)
        else {
            return Err(anyhow!(
                "benchmark case not found by id or task_id: {requested_id}"
            ));
        };
        selected.push(case.clone());
    }
    Ok(selected)
}

fn copy_asset(source_root: &Path, asset_path: &Path, workspace: &Path, task_id: &str) -> Result<()> {
    let relative = asset_path.strip_prefix(source_root).unwrap_or(asset_path);
    let target = workspace.join(relative);
    copy_path(asset_path, &target)?;

    if let Ok(rest) = relative.strip_prefix(Path::new("assets").join(task_id)) {
        if !rest.as_os_str().is_empty() {
            let flat_target = workspace.join(rest);
            if flat_target != target {
                copy_path(asset_path, &flat_target)?;
            }
        }
    }
    Ok(())
}

fn copy_path(from: &Path, to: &Path) -> Result<()> {
    if from.is_dir() {
        copy_dir_recursive(from, to)
    } else {
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(from, to).with_context(|| format!("failed to copy asset {}", from.display()))?;
        Ok(())
    }
}

fn copy_dir_recursive(from: &Path, to: &Path) -> Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir_recursive(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CaseTrace {
    run: BenchmarkRun,
    case: BenchmarkCase,
    result: BenchmarkResult,
    metrics: Option<BenchmarkMetricSnapshot>,
    events: Vec<TraceEvent>,
    grader_artifacts: Vec<TraceGraderArtifact>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TraceEvent {
    id: String,
    step: i64,
    kind: String,
    content: String,
    meta: Option<serde_json::Value>,
    created_at: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TraceGraderArtifact {
    id: String,
    grader_kind: String,
    command: Vec<String>,
    exit_code: Option<i32>,
    stdout_json: Option<serde_json::Value>,
    stderr: String,
    duration_ms: i64,
    created_at: String,
}

fn export_case_trace(
    db: &Database,
    run: &BenchmarkRun,
    case: &BenchmarkCase,
    result_id: &str,
    trace_root: &Path,
) -> Result<()> {
    let result = db
        .with_conn(|conn| queries::get_benchmark_result(conn, result_id))?
        .ok_or_else(|| anyhow!("benchmark result not found: {result_id}"))?;
    let result = benchmark_result_from_row(result)?;
    let events = if let Some(agent_id) = &result.agent_id {
        db.with_conn(|conn| queries::get_events_for_agent(conn, agent_id))?
            .into_iter()
            .map(trace_event_from_row)
            .collect::<Result<Vec<_>>>()?
    } else {
        Vec::new()
    };
    let metrics = db
        .with_conn(|conn| queries::get_case_metric_snapshot_for_result(conn, result_id))?
        .map(benchmark_metric_from_row)
        .transpose()?;
    let grader_artifacts = db
        .with_conn(|conn| queries::list_benchmark_grader_artifacts(conn, result_id))?
        .into_iter()
        .map(trace_grader_artifact_from_row)
        .collect::<Result<Vec<_>>>()?;
    let trace = CaseTrace {
        run: run.clone(),
        case: case.clone(),
        result,
        metrics,
        events,
        grader_artifacts,
    };
    let trace_dir = trace_root
        .join(&run.id)
        .join(safe_path_segment(&case.task_id));
    fs::create_dir_all(&trace_dir)
        .with_context(|| format!("failed to create trace dir {}", trace_dir.display()))?;
    fs::write(
        trace_dir.join("trace.json"),
        serde_json::to_string_pretty(&trace)?,
    )?;
    fs::write(
        trace_dir.join("trace.md"),
        export_case_trace_markdown(&trace),
    )?;
    fs::write(trace_dir.join("prompt.txt"), &trace.case.prompt)?;
    fs::write(
        trace_dir.join("final_response.txt"),
        trace.result.final_response.as_deref().unwrap_or_default(),
    )?;
    if let Some(metrics) = &trace.metrics {
        fs::write(
            trace_dir.join("metrics.json"),
            serde_json::to_string_pretty(&metrics.metrics)?,
        )?;
    }
    for (idx, artifact) in trace.grader_artifacts.iter().enumerate() {
        let prefix = format!("grader-{idx:02}");
        if let Some(stdout_json) = &artifact.stdout_json {
            fs::write(
                trace_dir.join(format!("{prefix}-stdout.json")),
                serde_json::to_string_pretty(stdout_json)?,
            )?;
        }
        fs::write(
            trace_dir.join(format!("{prefix}-stderr.txt")),
            &artifact.stderr,
        )?;
    }
    Ok(())
}

fn trace_event_from_row(row: queries::EventRow) -> Result<TraceEvent> {
    Ok(TraceEvent {
        id: row.id,
        step: row.step,
        kind: row.kind,
        content: row.content,
        meta: row.meta.map(|s| serde_json::from_str(&s)).transpose()?,
        created_at: row.created_at,
    })
}

fn trace_grader_artifact_from_row(
    row: queries::BenchmarkGraderArtifactRow,
) -> Result<TraceGraderArtifact> {
    Ok(TraceGraderArtifact {
        id: row.id,
        grader_kind: row.grader_kind,
        command: serde_json::from_str(&row.command_json)?,
        exit_code: row.exit_code,
        stdout_json: row
            .stdout_json
            .map(|s| serde_json::from_str(&s))
            .transpose()?,
        stderr: row.stderr,
        duration_ms: row.duration_ms,
        created_at: row.created_at,
    })
}

fn export_case_trace_markdown(trace: &CaseTrace) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Case Trace: {}\n\n", trace.case.task_id));
    out.push_str("## Result\n\n");
    out.push_str(&format!("- Run: {}\n", trace.run.id));
    out.push_str(&format!("- Case ID: {}\n", trace.case.id));
    out.push_str(&format!(
        "- Agent ID: {}\n",
        trace.result.agent_id.as_deref().unwrap_or("n/a")
    ));
    out.push_str(&format!("- Status: {}\n", trace.result.status));
    out.push_str(&format!(
        "- Success: {}\n",
        trace
            .result
            .success
            .map(|v| v.to_string())
            .unwrap_or_else(|| "ungraded".to_string())
    ));
    out.push_str(&format!("- Grading: {}\n", trace.result.grading_status));
    out.push_str(&format!(
        "- Workspace: {}\n\n",
        trace.result.workspace_path.as_deref().unwrap_or("n/a")
    ));
    if let Some(metrics) = &trace.metrics {
        let m = &metrics.metrics;
        out.push_str("## Metrics\n\n");
        out.push_str(&format!(
            "- Tokens: {} input + {} output = {} total\n",
            m.input_tokens, m.output_tokens, m.total_tokens
        ));
        out.push_str(&format!("- Cost USD: {:.6}\n", m.cost_usd));
        out.push_str(&format!("- LLM requests: {}\n", m.llm_request_count));
        out.push_str(&format!(
            "- Tool calls/results/errors: {}/{}/{}\n",
            m.tool_call_count, m.tool_result_count, m.tool_error_count
        ));
        out.push_str(&format!(
            "- Tool error rate: {}\n\n",
            m.tool_error_rate
                .map(|v| format!("{v:.4}"))
                .unwrap_or_else(|| "n/a".to_string())
        ));
    }
    out.push_str("## Prompt\n\n```text\n");
    out.push_str(&trace.case.prompt);
    out.push_str("\n```\n\n");
    out.push_str("## Final Response\n\n```text\n");
    out.push_str(trace.result.final_response.as_deref().unwrap_or_default());
    out.push_str("\n```\n\n");
    out.push_str("## Grader Artifacts\n\n");
    if trace.grader_artifacts.is_empty() {
        out.push_str("No grader artifacts.\n\n");
    } else {
        for artifact in &trace.grader_artifacts {
            out.push_str(&format!("### {} {}\n\n", artifact.grader_kind, artifact.id));
            out.push_str(&format!(
                "- Exit code: {}\n",
                artifact
                    .exit_code
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "n/a".to_string())
            ));
            out.push_str(&format!("- Duration ms: {}\n", artifact.duration_ms));
            out.push_str(&format!("- Command: `{}`\n", artifact.command.join(" ")));
            if let Some(stdout_json) = &artifact.stdout_json {
                out.push_str("\nStdout JSON:\n\n```json\n");
                out.push_str(
                    &serde_json::to_string_pretty(stdout_json)
                        .unwrap_or_else(|_| stdout_json.to_string()),
                );
                out.push_str("\n```\n");
            }
            if !artifact.stderr.trim().is_empty() {
                out.push_str("\nStderr:\n\n```text\n");
                out.push_str(&artifact.stderr);
                out.push_str("\n```\n");
            }
            out.push('\n');
        }
    }
    out.push_str("## Agent Events\n\n");
    for event in &trace.events {
        out.push_str(&format!(
            "### Step {} · {} · {}\n\n",
            event.step, event.kind, event.created_at
        ));
        if let Some(meta) = &event.meta {
            out.push_str("Meta:\n\n```json\n");
            out.push_str(&serde_json::to_string_pretty(meta).unwrap_or_else(|_| meta.to_string()));
            out.push_str("\n```\n\n");
        }
        out.push_str("Content:\n\n```text\n");
        out.push_str(&event.content);
        out.push_str("\n```\n\n");
    }
    out
}

fn safe_path_segment(value: &str) -> String {
    let segment = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    if segment.is_empty() {
        "case".to_string()
    } else {
        segment
    }
}

fn latest_final_response(db: &Database, agent_id: &str) -> Result<Option<String>> {
    db.with_conn(|conn| {
        let completion_summary = queries::latest_agent_message(conn, agent_id)?;
        if completion_summary
            .as_ref()
            .is_some_and(|s| !s.trim().is_empty())
        {
            Ok(completion_summary)
        } else {
            queries::latest_assistant_text(conn, agent_id)
        }
    })
}

fn benchmark_suite_from_row(row: queries::BenchmarkSuiteRow) -> Result<BenchmarkSuite> {
    Ok(BenchmarkSuite {
        id: row.id,
        name: row.name,
        description: row.description,
        source_kind: row.source_kind,
        source_path: row.source_path,
        source_ref: row.source_ref,
        manifest_json: serde_json::from_str(&row.manifest_json)?,
        case_count: row.case_count,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}

fn benchmark_case_from_row(row: queries::BenchmarkCaseRow) -> Result<BenchmarkCase> {
    Ok(BenchmarkCase {
        id: row.id,
        suite_id: row.suite_id,
        task_id: row.task_id,
        task_type: row.task_type,
        source_suite: row.source_suite,
        target_tool_or_capability: row.target_tool_or_capability,
        prompt: row.prompt,
        assets: serde_json::from_str(&row.assets_json)?,
        expected_outputs: serde_json::from_str(&row.expected_outputs_json)?,
        grader: row
            .grader_json
            .map(|s| serde_json::from_str(&s))
            .transpose()?,
        expected_output: row.expected_output,
        raw_json: serde_json::from_str(&row.raw_json)?,
        case_hash: row.case_hash,
        created_at: row.created_at,
    })
}

fn benchmark_run_from_row(row: queries::BenchmarkRunRow) -> Result<BenchmarkRun> {
    Ok(BenchmarkRun {
        id: row.id,
        suite_id: row.suite_id,
        name: row.name,
        status: row.status,
        agent_kind: row.agent_kind,
        provider: row.provider,
        model: row.model,
        base_url_hash: row.base_url_hash,
        agent_config_json: serde_json::from_str(&row.agent_config_json)?,
        git_commit: row.git_commit,
        git_dirty: row.git_dirty,
        benchmark_source_path: row.benchmark_source_path,
        case_ids: serde_json::from_str(&row.case_ids_json)?,
        timeout_seconds: row.timeout_seconds,
        max_steps: row.max_steps,
        token_budget: row.token_budget,
        cost_budget_usd: row.cost_budget_usd,
        workspace_root: row.workspace_root,
        metadata_json: serde_json::from_str(&row.metadata_json)?,
        started_at: row.started_at,
        completed_at: row.completed_at,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}

fn benchmark_result_from_row(row: queries::BenchmarkResultRow) -> Result<BenchmarkResult> {
    Ok(BenchmarkResult {
        id: row.id,
        run_id: row.run_id,
        case_id: row.case_id,
        agent_id: row.agent_id,
        workspace_path: row.workspace_path,
        status: row.status,
        success: row.success,
        grading_status: row.grading_status,
        final_response: row.final_response,
        artifact_refs: serde_json::from_str(&row.artifact_refs_json)?,
        error_message: row.error_message,
        started_at: row.started_at,
        completed_at: row.completed_at,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}

fn benchmark_metric_from_row(
    row: queries::BenchmarkMetricSnapshotRow,
) -> Result<BenchmarkMetricSnapshot> {
    Ok(BenchmarkMetricSnapshot {
        id: row.id,
        run_id: row.run_id,
        result_id: row.result_id,
        scope: row.scope,
        metrics: BenchmarkMetrics {
            input_tokens: row.input_tokens,
            output_tokens: row.output_tokens,
            total_tokens: row.total_tokens,
            cost_usd: row.cost_usd,
            llm_request_count: row.llm_request_count,
            tool_call_count: row.tool_call_count,
            tool_result_count: row.tool_result_count,
            tool_error_count: row.tool_error_count,
            tool_call_count_by_name: serde_json::from_str(&row.tool_call_count_by_name_json)?,
            runtime_ms: row.runtime_ms,
            successful_case_count: row.successful_case_count,
            graded_case_count: row.graded_case_count,
            total_case_count: row.total_case_count,
            all_cases_tsr: row.all_cases_tsr,
            graded_cases_tsr: row.graded_cases_tsr,
            token_per_success: row.token_per_success,
            tool_calls_per_success: row.tool_calls_per_success,
            requests_per_success: row.requests_per_success,
            tool_error_rate: row.tool_error_rate,
            guardrail_retry_count: row.guardrail_retry_count,
            recovery_attempt_count: row.recovery_attempt_count,
            read_only_loop_hint_count: row.read_only_loop_hint_count,
        },
        raw_json: serde_json::from_str(&row.raw_json)?,
        created_at: row.created_at,
    })
}

fn current_git_commit() -> Option<String> {
    let repo = git2::Repository::discover(std::env::current_dir().ok()?).ok()?;
    let head = repo.head().ok()?;
    head.target().map(|oid| oid.to_string())
}

fn current_git_dirty() -> bool {
    git2::Repository::discover(std::env::current_dir().unwrap_or_default())
        .and_then(|repo| repo.statuses(None).map(|statuses| !statuses.is_empty()))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn prepare_case_workspace_flattens_ga_case_assets_to_root() {
        let source = tempdir().unwrap();
        let workspace_root = tempdir().unwrap();
        let asset_dir = source.path().join("assets/teb_06_notebook_inspect");
        fs::create_dir_all(&asset_dir).unwrap();
        fs::write(asset_dir.join("analysis.ipynb"), "{}").unwrap();

        let case = BenchmarkCase {
            id: "case-id".to_string(),
            suite_id: "suite-id".to_string(),
            task_id: "teb_06_notebook_inspect".to_string(),
            task_type: "simple_tool_generalization".to_string(),
            source_suite: "claude_code".to_string(),
            target_tool_or_capability: "NotebookEdit".to_string(),
            prompt: "".to_string(),
            assets: vec!["assets/teb_06_notebook_inspect/analysis.ipynb".to_string()],
            expected_outputs: vec![],
            grader: None,
            expected_output: None,
            raw_json: serde_json::json!({}),
            case_hash: "hash".to_string(),
            created_at: "".to_string(),
        };

        let workspace = prepare_case_workspace(source.path(), &case, workspace_root.path()).unwrap();
        assert!(workspace.join("assets/teb_06_notebook_inspect/analysis.ipynb").exists());
        assert!(workspace.join("analysis.ipynb").exists());
    }
}
