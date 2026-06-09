//! FM-15 FR-09: Guardrail 完成检测。
//!
//! Coding Agent 通过调用 `task_complete(summary)` 工具显式声明完成。
//! AgentEngine 收到调用后，按 task.guardrails 顺序执行下面所有检查；
//! 任一失败则把失败信息注入为 user message 让 LLM 重试，直到重试预算耗尽。
//!
//! 当前 Phase 3 内置三种"硬"检查 + 一种"软"检查：
//! - `ArtifactsExist`：每个声明的 produces_artifacts 必须 publish 且文件真实存在
//! - `CommandPasses`：在 worktree 内执行 shell 命令，0 退出码视为通过
//! - `FilesNonEmpty`：glob 匹配后所有文件大小 > 0
//! - `LlmJudge`：通用 LLM 评判（P3-S3 单独接 LLM Provider）
//!
//! 为了让 P3-S1 单测自包含，本模块对 LLM Provider 是无感的：`LlmJudge`
//! 在这里只占位，真正调用 LLM 的逻辑由 P3-S3 的 `run_llm_judge` 提供。

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Guardrail {
    ArtifactsExist,
    CommandPasses {
        cmd: String,
        #[serde(default = "default_cmd_timeout")]
        timeout_sec: u32,
        #[serde(default)]
        working_dir: Option<String>,
    },
    FilesNonEmpty {
        globs: Vec<String>,
    },
    SummaryNonEmpty,
    SummaryJsonValid {
        #[serde(default)]
        require_non_empty: bool,
    },
    FilesJsonValid {
        globs: Vec<String>,
        #[serde(default)]
        require_non_empty: bool,
    },
    SummaryMatches {
        mode: SummaryMatchMode,
    },
    LlmJudge {
        criteria: String,
        #[serde(default)]
        model: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SummaryMatchMode {
    JsonCodeBlock,
    TextCodeBlock,
    ExactOk,
}

fn default_cmd_timeout() -> u32 {
    60
}

impl Guardrail {
    pub fn name(&self) -> &'static str {
        match self {
            Self::ArtifactsExist => "artifacts_exist",
            Self::CommandPasses { .. } => "command_passes",
            Self::FilesNonEmpty { .. } => "files_non_empty",
            Self::SummaryNonEmpty => "summary_non_empty",
            Self::SummaryJsonValid { .. } => "summary_json_valid",
            Self::FilesJsonValid { .. } => "files_json_valid",
            Self::SummaryMatches { .. } => "summary_matches",
            Self::LlmJudge { .. } => "llm_judge",
        }
    }
}

/// 单条 guardrail 的执行结果。
#[derive(Debug, Clone, Serialize)]
pub struct GuardrailReport {
    pub name: String,
    pub passed: bool,
    pub error: Option<String>,
}

/// 一轮 guardrails 的整体结果。
#[derive(Debug, Clone, Serialize)]
pub struct GuardrailRunResult {
    pub all_passed: bool,
    pub reports: Vec<GuardrailReport>,
}

impl GuardrailRunResult {
    /// 把失败原因渲染成给 LLM 看的 user message。
    pub fn format_failure_for_agent(&self) -> String {
        let mut out = String::from(
            "[Guardrail Check Failed]\n\
             You called task_complete, but the following guardrail checks did NOT pass. \
             Please fix the underlying issues and call task_complete again when ready.\n\n",
        );
        for r in &self.reports {
            if r.passed {
                out.push_str(&format!("- {} ✓ passed\n", r.name));
            } else {
                out.push_str(&format!(
                    "- {} ✗ FAILED: {}\n",
                    r.name,
                    r.error.as_deref().unwrap_or("unknown error")
                ));
            }
        }
        out
    }
}

/// 解析 task.guardrails (JSON 数组) → `Vec<Guardrail>`；
/// 解析失败一律返回空 vec，让 task 走默认的"调用 task_complete 即通过"路径。
pub fn parse_guardrails(json: &str) -> Vec<Guardrail> {
    serde_json::from_str(json).unwrap_or_default()
}

/// 执行 guardrails 时需要的所有外部上下文。
///
/// `produces`：从 task.produces_artifacts 解析出的 `(local_name, type)` 对，
/// `ArtifactsExist` 用它对账已 published 的 artifacts。
///
/// `llm` / `default_model`：给 `LlmJudge` 调用 LLM 用；调用方未提供时 `LlmJudge`
/// 视为 warn + pass（保持向后兼容）。
pub struct GuardrailContext<'a> {
    pub task_id: &'a str,
    pub mission_id: &'a str,
    pub repo_root: &'a Path,
    pub expected_output: Option<String>,
    pub produces: Vec<(String, String)>,
    pub task_description: Option<String>,
    pub completion_summary: Option<String>,
    pub llm: Option<std::sync::Arc<dyn crate::llm::LlmProvider>>,
    pub default_model: Option<String>,
}

/// 顺序执行所有 guardrails。
///
/// LlmJudge 行为：
/// - `ctx.llm` 提供 → 真实调用 LLM 评判
/// - `ctx.llm` 缺失 → warn + pass（向后兼容；单测不需要 mock provider）
pub async fn run_guardrails(
    guardrails: &[Guardrail],
    ctx: &GuardrailContext<'_>,
    db: &crate::db::Database,
) -> GuardrailRunResult {
    let mut reports = Vec::with_capacity(guardrails.len());
    for g in guardrails {
        reports.push(run_single(g, ctx, db).await);
    }
    GuardrailRunResult {
        all_passed: reports.iter().all(|r| r.passed),
        reports,
    }
}

async fn run_single(
    guardrail: &Guardrail,
    ctx: &GuardrailContext<'_>,
    db: &crate::db::Database,
) -> GuardrailReport {
    let name = guardrail.name().to_string();
    let result: Result<()> = match guardrail {
        Guardrail::ArtifactsExist => check_artifacts_exist(ctx, db).await,
        Guardrail::CommandPasses {
            cmd,
            timeout_sec,
            working_dir,
        } => check_command_passes(cmd, *timeout_sec, working_dir.as_deref(), ctx).await,
        Guardrail::FilesNonEmpty { globs } => check_files_non_empty(globs, ctx),
        Guardrail::SummaryNonEmpty => check_summary_non_empty(ctx),
        Guardrail::SummaryJsonValid { require_non_empty } => {
            check_summary_json_valid(*require_non_empty, ctx)
        }
        Guardrail::FilesJsonValid {
            globs,
            require_non_empty,
        } => check_files_json_valid(globs, *require_non_empty, ctx),
        Guardrail::SummaryMatches { mode } => check_summary_matches(mode, ctx),
        Guardrail::LlmJudge { criteria, model } => {
            check_llm_judge(criteria, model.as_deref(), ctx).await
        }
    };
    match result {
        Ok(()) => GuardrailReport {
            name,
            passed: true,
            error: None,
        },
        Err(e) => GuardrailReport {
            name,
            passed: false,
            error: Some(e.to_string()),
        },
    }
}

// ---- LlmJudge ----

/// 用 LLM 评判任务是否满足 `criteria`。
///
/// LLM 输入：任务描述 + 期望输出 + completion summary + 已发布 artifacts 概要 + criteria。
/// LLM 输出：JSON `{"passed": bool, "reason": "..."}`。解析失败 / 缺 provider 时 warn + pass。
async fn check_llm_judge(
    criteria: &str,
    model_override: Option<&str>,
    ctx: &GuardrailContext<'_>,
) -> Result<()> {
    use crate::llm::{ContentBlock, LlmRequest, Message, MessageRole};

    let provider = match ctx.llm.as_ref() {
        Some(p) => p.clone(),
        None => {
            tracing::warn!(
                "Guardrail::LlmJudge: no LLM provider in context, skipping (treating as pass)"
            );
            return Ok(());
        }
    };
    let model = model_override
        .map(|s| s.to_string())
        .or_else(|| ctx.default_model.clone())
        .unwrap_or_else(|| "default".to_string());

    let task_desc = ctx.task_description.as_deref().unwrap_or("(none)");
    let expected = ctx.expected_output.as_deref().unwrap_or("(none)");
    let summary = ctx.completion_summary.as_deref().unwrap_or("(none)");

    let user_text = format!(
        "## Task Description\n{task_desc}\n\n## Expected Output\n{expected}\n\n## Agent's Completion Summary\n{summary}\n\n## Acceptance Criteria\n{criteria}\n\nReturn ONLY a JSON object with two fields: `passed` (boolean) and `reason` (short string explaining why). No other text, no markdown fences."
    );

    let req = LlmRequest {
        model,
        system: Some(
            "You are a strict QA reviewer. Given a task description, expected output, the agent's \
             completion summary, and acceptance criteria, decide whether the task meets the criteria. \
             You must respond with a JSON object exactly: {\"passed\": <bool>, \"reason\": \"<short>\"}.\n\
             Be conservative: if the summary is vague, off-topic, or doesn't address the criteria, \
             return passed=false."
                .to_string(),
        ),
        messages: vec![Message {
            role: MessageRole::User,
            content: vec![ContentBlock::Text { text: user_text }],
            cache_control: None,
        }],
        tools: Vec::new(),
        max_tokens: 512,
        provider_extras: None,
    };

    let resp = provider
        .chat(&req)
        .await
        .with_context(|| "LlmJudge provider chat failed")?;
    let text = resp
        .content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");
    let parsed = parse_judge_response(&text).with_context(|| {
        format!(
            "LlmJudge: failed to parse model output: {}",
            truncate(&text, 400)
        )
    })?;
    if parsed.passed {
        Ok(())
    } else {
        Err(anyhow!(
            "LlmJudge says criteria NOT satisfied: {}",
            parsed.reason
        ))
    }
}

#[derive(Debug, Deserialize)]
struct LlmJudgeVerdict {
    passed: bool,
    #[serde(default)]
    reason: String,
}

fn parse_judge_response(text: &str) -> Result<LlmJudgeVerdict> {
    let json_slice = extract_json_value_slice(text)?;
    let v: LlmJudgeVerdict = serde_json::from_str(json_slice)
        .with_context(|| format!("invalid JSON: {}", truncate(json_slice, 400)))?;
    Ok(v)
}

fn extract_json_value_slice(text: &str) -> Result<&str> {
    let trimmed = text.trim();
    let body = if trimmed.starts_with("```") {
        let after_first_nl = trimmed
            .find('\n')
            .map(|i| &trimmed[i + 1..])
            .unwrap_or(trimmed);
        let close = after_first_nl.rfind("```").unwrap_or(after_first_nl.len());
        after_first_nl[..close].trim()
    } else {
        trimmed
    };
    let object_start = body.find('{');
    let array_start = body.find('[');
    let (start, end_char) = match (object_start, array_start) {
        (Some(o), Some(a)) if a < o => (a, ']'),
        (Some(o), _) => (o, '}'),
        (None, Some(a)) => (a, ']'),
        (None, None) => {
            if body == "null" || body.starts_with('"') {
                return Ok(body);
            }
            anyhow::bail!("no JSON object, array, string, or null in response")
        }
    };
    let end = body
        .rfind(end_char)
        .ok_or_else(|| anyhow!("no closing `{end_char}` in response"))?;
    if end < start {
        anyhow::bail!("malformed JSON delimiters");
    }
    Ok(&body[start..=end])
}

fn json_value_is_empty(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Null => true,
        serde_json::Value::String(s) => s.trim().is_empty(),
        serde_json::Value::Array(a) => a.is_empty(),
        serde_json::Value::Object(o) => o.is_empty(),
        _ => false,
    }
}

// ---- ArtifactsExist ----

async fn check_artifacts_exist(ctx: &GuardrailContext<'_>, db: &crate::db::Database) -> Result<()> {
    if ctx.produces.is_empty() {
        return Ok(());
    }
    let task_id = ctx.task_id.to_string();
    let rows = db
        .with_conn(move |conn| crate::db::queries::list_artifacts_for_task(conn, &task_id))
        .map_err(|e| anyhow!("db query failed: {e}"))?;

    let published: Vec<&crate::db::queries::ArtifactRow> =
        rows.iter().filter(|r| r.published).collect();

    let mut missing = Vec::new();
    for (name, ty) in &ctx.produces {
        let hit = published
            .iter()
            .any(|r| r.local_name == *name && r.artifact_type == *ty);
        if !hit {
            missing.push(format!("{name}:{ty}"));
        }
    }
    if !missing.is_empty() {
        return Err(anyhow!(
            "missing published artifacts: [{}]. Use the `publish_artifact` tool to declare each one before calling task_complete",
            missing.join(", ")
        ));
    }

    for row in published {
        let paths: Vec<String> = serde_json::from_str(&row.file_paths).unwrap_or_default();
        if paths.is_empty() {
            return Err(anyhow!(
                "artifact `{}` was published with empty file_paths",
                row.local_name
            ));
        }
        for p in paths {
            let full = ctx.repo_root.join(&p);
            if !full.exists() {
                return Err(anyhow!(
                    "artifact `{}` claims file `{}` but it does not exist on disk",
                    row.local_name,
                    p
                ));
            }
        }
    }
    Ok(())
}

// ---- CommandPasses ----

async fn check_command_passes(
    cmd: &str,
    timeout_sec: u32,
    working_dir: Option<&str>,
    ctx: &GuardrailContext<'_>,
) -> Result<()> {
    use tokio::process::Command;
    let cwd = match working_dir {
        Some(rel) => ctx.repo_root.join(rel),
        None => ctx.repo_root.to_path_buf(),
    };
    let fut = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .current_dir(&cwd)
        .env(
            "MIRAGENTY_TASK_COMPLETE_SUMMARY",
            ctx.completion_summary.as_deref().unwrap_or(""),
        )
        .output();
    let timeout_dur = Duration::from_secs(timeout_sec.max(1) as u64);
    let output = tokio::time::timeout(timeout_dur, fut)
        .await
        .map_err(|_| anyhow!("command timed out after {timeout_sec}s: `{cmd}`"))?
        .with_context(|| format!("failed to spawn command `{cmd}`"))?;
    if output.status.success() {
        return Ok(());
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(anyhow!(
        "command failed (exit {:?}); command_len={} command_hash16={}\n[stdout_tail]\n{}\n[stderr_tail]\n{}",
        output.status.code(),
        cmd.chars().count(),
        hash16(cmd),
        truncate_tail(&stdout, 1200),
        truncate_tail(&stderr, 1200)
    ))
}

fn hash16(s: &str) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn truncate_tail(s: &str, n: usize) -> String {
    let char_count = s.chars().count();
    if char_count <= n {
        s.to_string()
    } else {
        let mut out = format!("…[omitted {} chars]\n", char_count - n);
        out.extend(s.chars().skip(char_count - n));
        out
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(n).collect();
        out.push('…');
        out
    }
}

// ---- FilesNonEmpty ----

fn check_files_non_empty(globs: &[String], ctx: &GuardrailContext<'_>) -> Result<()> {
    use std::fs;
    if globs.is_empty() {
        return Ok(());
    }
    for g in globs {
        let pattern = ctx.repo_root.join(g);
        let pattern_str = pattern.to_string_lossy().to_string();
        let mut any_match = false;
        for entry in glob::glob(&pattern_str).map_err(|e| anyhow!("invalid glob `{g}`: {e}"))? {
            let path = entry.map_err(|e| anyhow!("glob iter err for `{g}`: {e}"))?;
            let metadata =
                fs::metadata(&path).with_context(|| format!("stat `{}` failed", path.display()))?;
            if !metadata.is_file() {
                continue;
            }
            any_match = true;
            if metadata.len() == 0 {
                return Err(anyhow!("file `{}` is empty", path.display()));
            }
        }
        if !any_match {
            return Err(anyhow!("no files match glob `{g}`"));
        }
    }
    Ok(())
}

fn check_summary_non_empty(ctx: &GuardrailContext<'_>) -> Result<()> {
    let summary = ctx.completion_summary.as_deref().unwrap_or("").trim();
    if summary.is_empty() {
        anyhow::bail!("task_complete.summary is empty; provide the actual final answer or artifact summary, not a blank completion")
    }
    Ok(())
}

fn check_summary_json_valid(require_non_empty: bool, ctx: &GuardrailContext<'_>) -> Result<()> {
    let summary = ctx.completion_summary.as_deref().unwrap_or("").trim();
    if summary.is_empty() {
        anyhow::bail!("task_complete.summary is empty but valid JSON was required")
    }
    let json_slice = extract_json_value_slice(summary)
        .with_context(|| "task_complete.summary must contain the requested JSON value")?;
    let value: serde_json::Value = serde_json::from_str(json_slice).with_context(|| {
        format!(
            "task_complete.summary contains invalid JSON: {}",
            truncate(json_slice, 400)
        )
    })?;
    if require_non_empty && json_value_is_empty(&value) {
        anyhow::bail!("task_complete.summary JSON is structurally empty; fill it with the real required fields or values before completing")
    }
    Ok(())
}

fn check_files_json_valid(
    globs: &[String],
    require_non_empty: bool,
    ctx: &GuardrailContext<'_>,
) -> Result<()> {
    use std::fs;
    if globs.is_empty() {
        return Ok(());
    }
    for g in globs {
        let pattern = ctx.repo_root.join(g);
        let pattern_str = pattern.to_string_lossy().to_string();
        let mut any_match = false;
        for entry in glob::glob(&pattern_str).map_err(|e| anyhow!("invalid glob `{g}`: {e}"))? {
            let path = entry.map_err(|e| anyhow!("glob iter err for `{g}`: {e}"))?;
            let metadata =
                fs::metadata(&path).with_context(|| format!("stat `{}` failed", path.display()))?;
            if !metadata.is_file() {
                continue;
            }
            any_match = true;
            if metadata.len() == 0 {
                anyhow::bail!("JSON file `{}` is empty", path.display());
            }
            let text = fs::read_to_string(&path)
                .with_context(|| format!("read JSON file `{}` failed", path.display()))?;
            let value: serde_json::Value = serde_json::from_str(&text)
                .with_context(|| format!("file `{}` is not valid JSON", path.display()))?;
            if require_non_empty && json_value_is_empty(&value) {
                anyhow::bail!("JSON file `{}` is structurally empty; fill it with the required data before completing", path.display());
            }
        }
        if !any_match {
            return Err(anyhow!("no files match JSON glob `{g}`"));
        }
    }
    Ok(())
}

// ---- SummaryMatches ----

fn check_summary_matches(mode: &SummaryMatchMode, ctx: &GuardrailContext<'_>) -> Result<()> {
    let summary = ctx.completion_summary.as_deref().unwrap_or("").trim();
    match mode {
        SummaryMatchMode::JsonCodeBlock => {
            if !summary.contains("```json") || !summary.contains("```") {
                return Err(anyhow!(
                    "task_complete.summary must be the final JSON code block requested by the task, including ```json fences; do not summarize what you did"
                ));
            }
        }
        SummaryMatchMode::TextCodeBlock => {
            if !summary.contains("```text") || !summary.contains("```") {
                return Err(anyhow!(
                    "task_complete.summary must be the final text code block requested by the task, including ```text fences; do not summarize what you did"
                ));
            }
        }
        SummaryMatchMode::ExactOk => {
            if summary != "OK" {
                return Err(anyhow!(
                    "task_complete.summary must be exactly OK with no extra explanation"
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use std::fs;
    use tempfile::TempDir;

    fn setup_db_with_mission_task() -> (Database, String, String) {
        let db = Database::open_in_memory().unwrap();
        let mission_id = "m-1".to_string();
        let task_id = "t-1".to_string();
        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO missions (id, title, description, status, repo_path) VALUES (?1, ?2, ?3, 'planned', '/tmp')",
                rusqlite::params![mission_id, "M", "d"],
            )?;
            conn.execute(
                "INSERT INTO tasks (id, mission_id, title, description, status) VALUES (?1, ?2, 't', 'd', 'running')",
                rusqlite::params![task_id, mission_id],
            )?;
            Ok(())
        })
        .unwrap();
        (db, mission_id, task_id)
    }

    #[test]
    fn parse_guardrails_round_trip() {
        let json = r#"[
            {"type":"artifacts_exist"},
            {"type":"command_passes","cmd":"echo hi","timeout_sec":5},
            {"type":"files_non_empty","globs":["src/**/*.rs"]},
            {"type":"summary_non_empty"},
            {"type":"summary_json_valid","require_non_empty":true},
            {"type":"files_json_valid","globs":["*.json"],"require_non_empty":true},
            {"type":"summary_matches","mode":"json_code_block"},
            {"type":"llm_judge","criteria":"is the code idiomatic?"}
        ]"#;
        let g = parse_guardrails(json);
        assert_eq!(g.len(), 8);
        assert_eq!(g[0].name(), "artifacts_exist");
        assert_eq!(g[1].name(), "command_passes");
        assert_eq!(g[2].name(), "files_non_empty");
        assert_eq!(g[3].name(), "summary_non_empty");
        assert_eq!(g[4].name(), "summary_json_valid");
        assert_eq!(g[5].name(), "files_json_valid");
        assert_eq!(g[6].name(), "summary_matches");
        assert_eq!(g[7].name(), "llm_judge");
    }

    #[tokio::test]
    async fn summary_matches_rejects_meta_summary_for_json_block() {
        let (db, mid, tid) = setup_db_with_mission_task();
        let dir = TempDir::new().unwrap();
        let ctx = GuardrailContext {
            task_id: &tid,
            mission_id: &mid,
            repo_root: dir.path(),
            expected_output: None,
            produces: vec![],
            task_description: None,
            completion_summary: Some("I found the files and output JSON.".to_string()),
            llm: None,
            default_model: None,
        };
        let r = run_guardrails(
            &[Guardrail::SummaryMatches {
                mode: SummaryMatchMode::JsonCodeBlock,
            }],
            &ctx,
            &db,
        )
        .await;
        assert!(!r.all_passed);
    }

    #[tokio::test]
    async fn summary_matches_accepts_json_block() {
        let (db, mid, tid) = setup_db_with_mission_task();
        let dir = TempDir::new().unwrap();
        let ctx = GuardrailContext {
            task_id: &tid,
            mission_id: &mid,
            repo_root: dir.path(),
            expected_output: None,
            produces: vec![],
            task_description: None,
            completion_summary: Some("```json\n{\"ok\": true}\n```".to_string()),
            llm: None,
            default_model: None,
        };
        let r = run_guardrails(
            &[Guardrail::SummaryMatches {
                mode: SummaryMatchMode::JsonCodeBlock,
            }],
            &ctx,
            &db,
        )
        .await;
        assert!(r.all_passed);
    }
    #[tokio::test]
    async fn summary_json_valid_rejects_empty_json() {
        let (db, mid, tid) = setup_db_with_mission_task();
        let dir = TempDir::new().unwrap();
        let ctx = GuardrailContext {
            task_id: &tid,
            mission_id: &mid,
            repo_root: dir.path(),
            expected_output: None,
            produces: vec![],
            task_description: None,
            completion_summary: Some("```json\n{}\n```".to_string()),
            llm: None,
            default_model: None,
        };
        let r = run_guardrails(
            &[Guardrail::SummaryJsonValid {
                require_non_empty: true,
            }],
            &ctx,
            &db,
        )
        .await;
        assert!(!r.all_passed);
        assert!(r.reports[0]
            .error
            .as_ref()
            .unwrap()
            .contains("structurally empty"));
    }

    #[tokio::test]
    async fn summary_json_valid_accepts_fenced_json() {
        let (db, mid, tid) = setup_db_with_mission_task();
        let dir = TempDir::new().unwrap();
        let ctx = GuardrailContext {
            task_id: &tid,
            mission_id: &mid,
            repo_root: dir.path(),
            expected_output: None,
            produces: vec![],
            task_description: None,
            completion_summary: Some("```json\n{\"ok\":true}\n```".to_string()),
            llm: None,
            default_model: None,
        };
        let r = run_guardrails(
            &[Guardrail::SummaryJsonValid {
                require_non_empty: true,
            }],
            &ctx,
            &db,
        )
        .await;
        assert!(r.all_passed, "{:?}", r.reports);
    }

    #[tokio::test]
    async fn files_json_valid_rejects_empty_object() {
        let (db, mid, tid) = setup_db_with_mission_task();
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("report.json"), "{}").unwrap();
        let ctx = GuardrailContext {
            task_id: &tid,
            mission_id: &mid,
            repo_root: dir.path(),
            expected_output: None,
            produces: vec![],
            task_description: None,
            completion_summary: None,
            llm: None,
            default_model: None,
        };
        let r = run_guardrails(
            &[Guardrail::FilesJsonValid {
                globs: vec!["report.json".to_string()],
                require_non_empty: true,
            }],
            &ctx,
            &db,
        )
        .await;
        assert!(!r.all_passed);
    }

    #[tokio::test]
    async fn files_json_valid_accepts_non_empty_json() {
        let (db, mid, tid) = setup_db_with_mission_task();
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("report.json"), "{\"answer\": 42}").unwrap();
        let ctx = GuardrailContext {
            task_id: &tid,
            mission_id: &mid,
            repo_root: dir.path(),
            expected_output: None,
            produces: vec![],
            task_description: None,
            completion_summary: None,
            llm: None,
            default_model: None,
        };
        let r = run_guardrails(
            &[Guardrail::FilesJsonValid {
                globs: vec!["report.json".to_string()],
                require_non_empty: true,
            }],
            &ctx,
            &db,
        )
        .await;
        assert!(r.all_passed, "{:?}", r.reports);
    }

    #[tokio::test]
    async fn artifacts_exist_passes_when_no_produces_declared() {
        let (db, mid, tid) = setup_db_with_mission_task();
        let dir = TempDir::new().unwrap();
        let ctx = GuardrailContext {
            task_id: &tid,
            mission_id: &mid,
            repo_root: dir.path(),
            expected_output: None,
            produces: vec![],
            task_description: None,
            completion_summary: None,
            llm: None,
            default_model: None,
        };
        let r = run_guardrails(&[Guardrail::ArtifactsExist], &ctx, &db).await;
        assert!(r.all_passed);
    }

    #[tokio::test]
    async fn artifacts_exist_fails_when_declared_but_not_published() {
        let (db, mid, tid) = setup_db_with_mission_task();
        let dir = TempDir::new().unwrap();
        let ctx = GuardrailContext {
            task_id: &tid,
            mission_id: &mid,
            repo_root: dir.path(),
            expected_output: None,
            produces: vec![("schema_dts".to_string(), "schema".to_string())],
            task_description: None,
            completion_summary: None,
            llm: None,
            default_model: None,
        };
        let r = run_guardrails(&[Guardrail::ArtifactsExist], &ctx, &db).await;
        assert!(!r.all_passed);
        let err = r.reports[0].error.as_ref().unwrap();
        assert!(err.contains("schema_dts:schema"), "got: {err}");
    }

    #[tokio::test]
    async fn artifacts_exist_passes_when_published_and_files_present() {
        let (db, mid, tid) = setup_db_with_mission_task();
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("schema.json");
        fs::write(&file, "{}").unwrap();

        // Insert a published artifact row with non-empty file_paths
        let mid_clone = mid.clone();
        let tid_clone = tid.clone();
        db.with_conn(move |conn| {
            conn.execute(
                "INSERT INTO artifacts (id, mission_id, producer_task_id, type, local_name, summary, file_paths, published)
                 VALUES (?1, ?2, ?3, 'schema', 'schema_dts', '', ?4, 1)",
                rusqlite::params!["t-1.schema_dts", mid_clone, tid_clone, "[\"schema.json\"]"],
            )?;
            Ok(())
        })
        .unwrap();

        let ctx = GuardrailContext {
            task_id: &tid,
            mission_id: &mid,
            repo_root: dir.path(),
            expected_output: None,
            produces: vec![("schema_dts".to_string(), "schema".to_string())],
            task_description: None,
            completion_summary: None,
            llm: None,
            default_model: None,
        };
        let r = run_guardrails(&[Guardrail::ArtifactsExist], &ctx, &db).await;
        assert!(r.all_passed, "reports: {:?}", r.reports);
    }

    #[tokio::test]
    async fn artifacts_exist_fails_when_file_missing() {
        let (db, mid, tid) = setup_db_with_mission_task();
        let dir = TempDir::new().unwrap();
        let mid_clone = mid.clone();
        let tid_clone = tid.clone();
        db.with_conn(move |conn| {
            conn.execute(
                "INSERT INTO artifacts (id, mission_id, producer_task_id, type, local_name, summary, file_paths, published)
                 VALUES (?1, ?2, ?3, 'schema', 'schema_dts', '', ?4, 1)",
                rusqlite::params!["t-1.schema_dts", mid_clone, tid_clone, "[\"missing.json\"]"],
            )?;
            Ok(())
        })
        .unwrap();
        let ctx = GuardrailContext {
            task_id: &tid,
            mission_id: &mid,
            repo_root: dir.path(),
            expected_output: None,
            produces: vec![("schema_dts".to_string(), "schema".to_string())],
            task_description: None,
            completion_summary: None,
            llm: None,
            default_model: None,
        };
        let r = run_guardrails(&[Guardrail::ArtifactsExist], &ctx, &db).await;
        assert!(!r.all_passed);
        assert!(r.reports[0]
            .error
            .as_ref()
            .unwrap()
            .contains("does not exist"));
    }

    #[tokio::test]
    async fn command_passes_succeeds_on_zero_exit() {
        let (db, mid, tid) = setup_db_with_mission_task();
        let dir = TempDir::new().unwrap();
        let g = Guardrail::CommandPasses {
            cmd: "echo ok".into(),
            timeout_sec: 5,
            working_dir: None,
        };
        let ctx = GuardrailContext {
            task_id: &tid,
            mission_id: &mid,
            repo_root: dir.path(),
            expected_output: None,
            produces: vec![],
            task_description: None,
            completion_summary: None,
            llm: None,
            default_model: None,
        };
        let r = run_guardrails(&[g], &ctx, &db).await;
        assert!(r.all_passed);
    }

    #[tokio::test]
    async fn command_passes_fails_on_non_zero_exit() {
        let (db, mid, tid) = setup_db_with_mission_task();
        let dir = TempDir::new().unwrap();
        let g = Guardrail::CommandPasses {
            cmd: "exit 2".into(),
            timeout_sec: 5,
            working_dir: None,
        };
        let ctx = GuardrailContext {
            task_id: &tid,
            mission_id: &mid,
            repo_root: dir.path(),
            expected_output: None,
            produces: vec![],
            task_description: None,
            completion_summary: None,
            llm: None,
            default_model: None,
        };
        let r = run_guardrails(&[g], &ctx, &db).await;
        assert!(!r.all_passed);
        let err = r.reports[0].error.as_ref().unwrap();
        assert!(err.contains("exit"));
        assert!(err.contains("command_len="));
        assert!(err.contains("command_hash16="));
        assert!(!err.contains("`exit 2`"));
    }

    #[tokio::test]
    async fn command_passes_omits_long_command_body_on_failure() {
        let (db, mid, tid) = setup_db_with_mission_task();
        let dir = TempDir::new().unwrap();
        let long_marker = "UNIQUE_LONG_GUARDRAIL_COMMAND_MARKER".repeat(80);
        let g = Guardrail::CommandPasses {
            cmd: format!("echo {long_marker} >&2; exit 9"),
            timeout_sec: 5,
            working_dir: None,
        };
        let ctx = GuardrailContext {
            task_id: &tid,
            mission_id: &mid,
            repo_root: dir.path(),
            expected_output: None,
            produces: vec![],
            task_description: None,
            completion_summary: None,
            llm: None,
            default_model: None,
        };
        let r = run_guardrails(&[g], &ctx, &db).await;
        assert!(!r.all_passed);
        let err = r.reports[0].error.as_ref().unwrap();
        assert!(err.contains("command_len="));
        assert!(err.contains("command_hash16="));
        assert!(err.contains("[stderr_tail]"));
        assert!(!err.contains(&long_marker));
    }

    #[tokio::test]
    async fn command_passes_times_out() {
        let (db, mid, tid) = setup_db_with_mission_task();
        let dir = TempDir::new().unwrap();
        let g = Guardrail::CommandPasses {
            cmd: "sleep 5".into(),
            timeout_sec: 1,
            working_dir: None,
        };
        let ctx = GuardrailContext {
            task_id: &tid,
            mission_id: &mid,
            repo_root: dir.path(),
            expected_output: None,
            produces: vec![],
            task_description: None,
            completion_summary: None,
            llm: None,
            default_model: None,
        };
        let r = run_guardrails(&[g], &ctx, &db).await;
        assert!(!r.all_passed);
        assert!(r.reports[0].error.as_ref().unwrap().contains("timed out"));
    }

    #[tokio::test]
    async fn files_non_empty_passes() {
        let (db, mid, tid) = setup_db_with_mission_task();
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("a.txt"), "non-empty").unwrap();
        let g = Guardrail::FilesNonEmpty {
            globs: vec!["a.txt".into()],
        };
        let ctx = GuardrailContext {
            task_id: &tid,
            mission_id: &mid,
            repo_root: dir.path(),
            expected_output: None,
            produces: vec![],
            task_description: None,
            completion_summary: None,
            llm: None,
            default_model: None,
        };
        let r = run_guardrails(&[g], &ctx, &db).await;
        assert!(r.all_passed, "{:?}", r.reports);
    }

    #[tokio::test]
    async fn files_non_empty_fails_on_empty_file() {
        let (db, mid, tid) = setup_db_with_mission_task();
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("a.txt"), "").unwrap();
        let g = Guardrail::FilesNonEmpty {
            globs: vec!["a.txt".into()],
        };
        let ctx = GuardrailContext {
            task_id: &tid,
            mission_id: &mid,
            repo_root: dir.path(),
            expected_output: None,
            produces: vec![],
            task_description: None,
            completion_summary: None,
            llm: None,
            default_model: None,
        };
        let r = run_guardrails(&[g], &ctx, &db).await;
        assert!(!r.all_passed);
        assert!(r.reports[0].error.as_ref().unwrap().contains("empty"));
    }

    #[tokio::test]
    async fn files_non_empty_fails_when_no_match() {
        let (db, mid, tid) = setup_db_with_mission_task();
        let dir = TempDir::new().unwrap();
        let g = Guardrail::FilesNonEmpty {
            globs: vec!["does/not/exist/*.rs".into()],
        };
        let ctx = GuardrailContext {
            task_id: &tid,
            mission_id: &mid,
            repo_root: dir.path(),
            expected_output: None,
            produces: vec![],
            task_description: None,
            completion_summary: None,
            llm: None,
            default_model: None,
        };
        let r = run_guardrails(&[g], &ctx, &db).await;
        assert!(!r.all_passed);
        assert!(r.reports[0]
            .error
            .as_ref()
            .unwrap()
            .contains("no files match"));
    }

    #[test]
    fn parse_judge_response_accepts_plain_json() {
        let v = parse_judge_response(r#"{"passed": true, "reason": "ok"}"#).unwrap();
        assert!(v.passed);
        assert_eq!(v.reason, "ok");
    }

    #[test]
    fn parse_judge_response_accepts_fenced_json() {
        let v = parse_judge_response(
            "```json\n{\"passed\": false, \"reason\": \"missing tests\"}\n```",
        )
        .unwrap();
        assert!(!v.passed);
        assert!(v.reason.contains("tests"));
    }

    #[test]
    fn parse_judge_response_accepts_extra_prose() {
        let v = parse_judge_response(
            "Here is my verdict:\n{\"passed\": true, \"reason\": \"ok\"}\nDone.",
        )
        .unwrap();
        assert!(v.passed);
    }

    #[test]
    fn parse_judge_response_rejects_non_json() {
        assert!(parse_judge_response("yes").is_err());
        assert!(parse_judge_response("").is_err());
    }

    #[tokio::test]
    async fn llm_judge_returns_pass_when_no_provider() {
        let (db, mid, tid) = setup_db_with_mission_task();
        let dir = TempDir::new().unwrap();
        let g = Guardrail::LlmJudge {
            criteria: "task is well-tested".into(),
            model: None,
        };
        let ctx = GuardrailContext {
            task_id: &tid,
            mission_id: &mid,
            repo_root: dir.path(),
            expected_output: None,
            produces: vec![],
            task_description: None,
            completion_summary: None,
            llm: None,
            default_model: None,
        };
        let r = run_guardrails(&[g], &ctx, &db).await;
        assert!(
            r.all_passed,
            "expected pass when no provider available, got: {:?}",
            r.reports
        );
    }

    #[tokio::test]
    async fn run_result_format_failure_string_lists_each_check() {
        let (db, mid, tid) = setup_db_with_mission_task();
        let dir = TempDir::new().unwrap();
        let g = vec![
            Guardrail::CommandPasses {
                cmd: "true".into(),
                timeout_sec: 5,
                working_dir: None,
            },
            Guardrail::CommandPasses {
                cmd: "exit 1".into(),
                timeout_sec: 5,
                working_dir: None,
            },
        ];
        let ctx = GuardrailContext {
            task_id: &tid,
            mission_id: &mid,
            repo_root: dir.path(),
            expected_output: None,
            produces: vec![],
            task_description: None,
            completion_summary: None,
            llm: None,
            default_model: None,
        };
        let r = run_guardrails(&g, &ctx, &db).await;
        assert!(!r.all_passed);
        let msg = r.format_failure_for_agent();
        assert!(msg.contains("✓ passed"));
        assert!(msg.contains("✗ FAILED"));
    }
}
