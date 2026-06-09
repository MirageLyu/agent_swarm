use anyhow::{anyhow, Context, Result};
use miragenty_lib::agent::approval::ApprovalCoordinator;
use miragenty_lib::benchmark::{
    export_summary_csv, export_summary_json, export_summary_markdown, import_and_run_benchmark,
    BenchmarkRunConfig,
};
use miragenty_lib::commands::{AppConfig, ConfigManager};
use miragenty_lib::db::Database;
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse()?;
    let pipeline = BenchmarkPipelineConfig::load(&args.config)?;
    let root = pipeline_root(&args.config)?;
    let data_dir = pipeline
        .data_dir
        .clone()
        .unwrap_or_else(|| root.join("data"));
    let workspace_root = args
        .workspace_root
        .clone()
        .or_else(|| pipeline.workspace_root.clone())
        .unwrap_or_else(|| root.join("workspaces"));
    let output_dir = args
        .output_dir
        .clone()
        .or_else(|| pipeline.output_dir.clone())
        .unwrap_or_else(|| root.join("reports"));

    std::fs::create_dir_all(&data_dir)
        .with_context(|| format!("failed to create data dir {}", data_dir.display()))?;
    std::fs::create_dir_all(&workspace_root).with_context(|| {
        format!(
            "failed to create workspace root {}",
            workspace_root.display()
        )
    })?;
    std::fs::create_dir_all(&output_dir)
        .with_context(|| format!("failed to create output dir {}", output_dir.display()))?;

    let app = tauri::Builder::default()
        .manage(Database::open(&data_dir)?)
        .manage(ConfigManager::from_config(
            pipeline.to_app_config(),
            args.config.clone(),
        ))
        .manage(ApprovalCoordinator::new())
        .build(tauri::generate_context!())?;

    let summary = import_and_run_benchmark(
        app.handle().clone(),
        args.suite,
        BenchmarkRunConfig {
            name: args.name,
            case_ids: args.case_ids,
            timeout_seconds: args.timeout_seconds.or(pipeline.timeout_seconds),
            max_steps: args.max_steps.or(pipeline.max_steps),
            token_budget: args.token_budget.or(pipeline.token_budget),
            cost_budget_usd: args.cost_budget_usd.or(pipeline.cost_budget_usd),
            workspace_root: Some(workspace_root.to_string_lossy().to_string()),
            trace_root: Some(output_dir.join("traces").to_string_lossy().to_string()),
        },
    )
    .await?;

    let stem = format!("benchmark-{}", summary.run.id);
    let json_path = output_dir.join(format!("{stem}.json"));
    let md_path = output_dir.join(format!("{stem}.md"));
    let csv_path = output_dir.join(format!("{stem}.csv"));
    std::fs::write(&json_path, export_summary_json(&summary)?)?;
    std::fs::write(&md_path, export_summary_markdown(&summary))?;
    std::fs::write(&csv_path, export_summary_csv(&summary))?;

    println!("Benchmark run completed");
    println!("run_id={}", summary.run.id);
    println!("status={}", summary.run.status);
    if let Some(metrics) = &summary.metrics {
        let m = &metrics.metrics;
        println!("cases={}", m.total_case_count.unwrap_or(0));
        println!("successful={}", m.successful_case_count.unwrap_or(0));
        println!("all_cases_tsr={:.4}", m.all_cases_tsr.unwrap_or(0.0));
        println!("total_tokens={}", m.total_tokens);
        println!("cost_usd={:.6}", m.cost_usd);
        println!("llm_requests={}", m.llm_request_count);
        println!("tool_calls={}", m.tool_call_count);
        println!("tool_errors={}", m.tool_error_count);
        println!("context_saved_chars={}", m.context_saved_chars);
        println!("tool_result_refs={}", m.tool_result_ref_count);
        println!("tool_result_repeats={}", m.tool_result_repeat_count);
        println!("evidence_read_refs={}", m.evidence_read_ref_count);
        println!("shell_content_commands={}", m.shell_content_command_count);
        println!("persisted_tool_results={}", m.persisted_tool_result_count);
        println!(
            "per_message_budget_replacements={}",
            m.per_message_budget_replacement_count
        );
        println!(
            "contract_validation_attempts={}",
            m.contract_validation_attempt_count
        );
        println!("contract_violations={}", m.contract_violation_count);
        println!("contract_repair_retries={}", m.contract_repair_retry_count);
    }
    println!("json={}", json_path.display());
    println!("markdown={}", md_path.display());
    println!("csv={}", csv_path.display());

    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BenchmarkPipelineConfig {
    provider: String,
    model: String,
    api_key: String,
    #[serde(default)]
    base_url: String,
    #[serde(default)]
    data_dir: Option<PathBuf>,
    #[serde(default)]
    workspace_root: Option<PathBuf>,
    #[serde(default)]
    output_dir: Option<PathBuf>,
    #[serde(default)]
    max_steps: Option<i64>,
    #[serde(default)]
    timeout_seconds: Option<i64>,
    #[serde(default)]
    max_output_tokens: Option<u32>,
    #[serde(default)]
    stream_network_retries: Option<u32>,
    #[serde(default)]
    stream_initial_retry_delay_ms: Option<u64>,
    #[serde(default)]
    step_idle_seconds: Option<u64>,
    #[serde(default)]
    token_budget: Option<i64>,
    #[serde(default)]
    cost_budget_usd: Option<f64>,
    #[serde(default)]
    fallback_model: Option<String>,
    #[serde(default)]
    fallback_sticky: Option<bool>,
}

impl BenchmarkPipelineConfig {
    fn load(path: &Path) -> Result<Self> {
        let data = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read benchmark config {}", path.display()))?;
        if path.extension().and_then(|s| s.to_str()) == Some("json") {
            Ok(serde_json::from_str(&data)?)
        } else {
            Ok(serde_yml::from_str(&data)?)
        }
    }

    fn to_app_config(&self) -> AppConfig {
        let mut config = AppConfig::default();
        config.provider = self.provider.clone();
        config.default_model = self.model.clone();
        config.base_url = self.base_url.clone();
        config
            .api_keys
            .insert(self.provider.clone(), self.api_key.clone());
        config
            .api_keys
            .insert("default".to_string(), self.api_key.clone());
        if let Some(v) = self.max_steps {
            config.max_agent_steps = v.max(1) as u32;
        }
        if let Some(v) = self.timeout_seconds {
            config.agent_timeout_seconds = v.max(1) as u64;
        }
        if let Some(v) = self.max_output_tokens {
            config.agent_max_output_tokens = v.max(1);
        }
        if let Some(v) = self.stream_network_retries {
            config.stream_network_retries = v;
        }
        if let Some(v) = self.stream_initial_retry_delay_ms {
            config.stream_initial_retry_delay_ms = v;
        }
        if let Some(v) = self.step_idle_seconds {
            config.agent_step_idle_seconds = v;
        }
        if let Some(v) = self.token_budget {
            config.agent_output_token_budget = v.max(0) as u64;
        }
        if let Some(v) = &self.fallback_model {
            config.agent_fallback_model = v.trim().to_string();
        }
        if let Some(v) = self.fallback_sticky {
            config.agent_fallback_sticky = v;
        }
        config
    }
}

#[derive(Debug)]
struct Args {
    suite: PathBuf,
    config: PathBuf,
    workspace_root: Option<PathBuf>,
    output_dir: Option<PathBuf>,
    name: Option<String>,
    case_ids: Option<Vec<String>>,
    timeout_seconds: Option<i64>,
    max_steps: Option<i64>,
    token_budget: Option<i64>,
    cost_budget_usd: Option<f64>,
}

impl Args {
    fn parse() -> Result<Self> {
        let mut suite = None;
        let mut config = None;
        let mut workspace_root = None;
        let mut output_dir = None;
        let mut name = None;
        let mut case_ids = None;
        let mut timeout_seconds = None;
        let mut max_steps = None;
        let mut token_budget = None;
        let mut cost_budget_usd = None;

        let mut iter = std::env::args().skip(1);
        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--suite" => suite = Some(next_path(&mut iter, "--suite")?),
                "--config" => config = Some(next_path(&mut iter, "--config")?),
                "--workspace-root" => {
                    workspace_root = Some(next_path(&mut iter, "--workspace-root")?)
                }
                "--output-dir" => output_dir = Some(next_path(&mut iter, "--output-dir")?),
                "--name" => name = Some(next_value(&mut iter, "--name")?),
                "--case-id" => {
                    case_ids
                        .get_or_insert_with(Vec::new)
                        .push(next_value(&mut iter, "--case-id")?);
                }
                "--timeout-seconds" => {
                    timeout_seconds = Some(next_parse(&mut iter, "--timeout-seconds")?)
                }
                "--max-steps" => max_steps = Some(next_parse(&mut iter, "--max-steps")?),
                "--token-budget" => token_budget = Some(next_parse(&mut iter, "--token-budget")?),
                "--cost-budget-usd" => {
                    cost_budget_usd = Some(next_parse(&mut iter, "--cost-budget-usd")?)
                }
                "--help" | "-h" => {
                    print_help();
                    std::process::exit(0);
                }
                _ if arg.starts_with('-') => return Err(anyhow!("unknown argument: {arg}")),
                _ if suite.is_none() => suite = Some(PathBuf::from(arg)),
                _ => return Err(anyhow!("unexpected positional argument: {arg}")),
            }
        }

        Ok(Self {
            suite: suite.ok_or_else(|| anyhow!("missing --suite <path>"))?,
            config: config.ok_or_else(|| anyhow!("missing --config <path>"))?,
            workspace_root,
            output_dir,
            name,
            case_ids,
            timeout_seconds,
            max_steps,
            token_budget,
            cost_budget_usd,
        })
    }
}

fn next_value(iter: &mut impl Iterator<Item = String>, flag: &str) -> Result<String> {
    iter.next()
        .ok_or_else(|| anyhow!("missing value for {flag}"))
}

fn next_path(iter: &mut impl Iterator<Item = String>, flag: &str) -> Result<PathBuf> {
    Ok(PathBuf::from(next_value(iter, flag)?))
}

fn next_parse<T>(iter: &mut impl Iterator<Item = String>, flag: &str) -> Result<T>
where
    T: std::str::FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    next_value(iter, flag)?.parse::<T>().map_err(Into::into)
}

fn pipeline_root(config_path: &Path) -> Result<PathBuf> {
    Ok(config_path
        .parent()
        .ok_or_else(|| anyhow!("config path has no parent: {}", config_path.display()))?
        .to_path_buf())
}

fn print_help() {
    println!(
        "Usage: cargo run --manifest-path src-tauri/Cargo.toml --bin benchmark_runner -- --config <path> --suite <path> [options]\n\n\
Config file (YAML or JSON):\n\
  provider: openai_compat | anthropic\n\
  model: <model-name>\n\
  apiKey: <key>\n\
  baseUrl: <openai-compatible-base-url>\n\n\
Options:\n\
  --config <path>            Independent benchmark pipeline config\n\
  --suite <path>             GA benchmark suite path or GA repo root\n\
  --workspace-root <path>    Root for isolated benchmark case workspaces\n\
  --output-dir <path>        Directory for JSON/Markdown/CSV reports\n\
  --name <name>              Benchmark run name\n\
  --case-id <id>             Case DB id to run; can be repeated\n\
  --timeout-seconds <n>      Per-case wall-clock timeout override\n\
  --max-steps <n>            Per-case max agent steps override\n\
  --token-budget <n>         Per-case output token budget override\n\
  --cost-budget-usd <n>      Run metadata cost budget\n"
    );
}
