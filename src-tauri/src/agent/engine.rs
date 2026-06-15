//! Coding Agent 执行引擎 (FM-15 Phase 3 重构版)。
//!
//! 关键变更（FR-09 / FR-11）：
//! - 完成检测从"无 tool_use 即完成"改为"必须调用 `task_complete` 工具"
//! - `task_complete` 触发 guardrails 顺序检查；失败则注入 user message 让 LLM 重试
//! - 重试预算耗尽 / 超时 / 步数超限 → 任务 failed（已修改文件仍 commit）
//! - 整个执行循环包裹 `tokio::time::timeout`，剩余 5 步时注入"请尽快收尾"提示
//!
//! 兼容性：当 task 没有任何 guardrails 配置时，guardrail run 仍会跑（结果为空 → 全部通过），
//! 等价于 Phase 2 行为；不会再因为 LLM "顺嘴说一句" 就误判完成。

use anyhow::Result;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tauri::{Emitter, Manager};
use tokio::sync::mpsc;
use tokio::time::{timeout, Duration};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::db::{queries, Database};
use crate::llm::{
    stream_chat_with_idle_guard_full, ContentBlock, LlmProvider, LlmRequest, Message, MessageRole,
    StreamChunk, StreamChunkKind, StreamGuardError, StreamRetryPolicy,
    DEFAULT_STREAM_IDLE_HEARTBEAT_SECS, DEFAULT_STREAM_IDLE_TIMEOUT,
};
use crate::tools::{coding_agent_tools_with_artifact_support, ToolExecutor, TASK_COMPLETE_TOOL};

// P0-2: BudgetStopReason 由 BudgetDecision 内部携带，主循环只 match Stop variant，
// 不直接构造 reason，所以这里不 import；`.as_str()` 通过 method dispatch 触达。
use super::budget_tracker::{BudgetDecision, BudgetTracker};
use super::codebase_intel;
use super::delivery::{
    task_complete_handoff_is_agent_authored, DeliveryArtifactRef, TaskHandoffPacket,
};
use super::guardrail::{self, Guardrail, GuardrailContext};
use super::recovery_log::{
    build_recovery_attempt_meta, build_recovery_succeeded_meta, format_attempt_content,
    format_succeeded_content, RecoveryStrategy, RecoveryTrigger,
};
use super::runtime_env;
use super::task_contract::{self, ContractContext, TaskContract};
use super::types::*;

/// FR-11 默认值；Scheduler 可从配置覆盖。
///
/// 1800s（30 min）是为了配合 Phase 4 的"卡住才算超时"策略：LLM 流式响应有 stream-idle
/// 兜底（默认 60s 静默就杀），shell_exec 有进程级 watchdog（默认 60s idle / 5min wall），
/// 这层 wall-clock 仅作为兜底防御无限循环；正常任务远到不了。
pub const DEFAULT_AGENT_TIMEOUT_SECS: u64 = 1800;
pub const DEFAULT_MAX_AGENT_STEPS: u32 = 80;
/// 当 LLM 连续 N 次不调用任何工具但又没调用 task_complete，就注入提示。
const MAX_CONSECUTIVE_NO_TOOL: u32 = 3;
/// L3 循环检测：连续 N 步只调用只读工具（read/search/list）就注入"开始动手"提示。
const READ_ONLY_LOOP_THRESHOLD: u32 = 5;
/// 步数距上限只剩 N 时注入"剩余 N 步"提示。
const STEPS_REMAINING_HINT: u32 = 5;
const ARTIFACT_FIRST_HINT_STEP: u32 = 3;
const ARTIFACT_CHECKPOINT_REMAINING_STEPS: u32 = 12;
const GUARDRAIL_PRECHECK_REMAINING_STEPS: u32 = 12;
const FINALIZATION_TIMEOUT_FRACTION: f64 = 0.85;
/// Issue 3: 单步 LLM 流被 idle watchdog 中止（卡住 180s）时，给 LLM 发"continue"
/// 重试的次数预算。耗尽则真失败。**Step 级**：每个 step 开始时重置。
///
/// 之前一次 IdleTimeout 就把整个 agent 标 failed，对于经常半截卡住的 reseller
/// （DeepSeek-V4 / SiliconFlow Qwen）非常痛。改成"卡住就 continue"，更接近用户
/// 在 Cursor / Claude Desktop 看到 "continue" 按钮的直觉。
///
/// 为什么 step 级而非任务级：任务级 budget 等于把"可恢复故障"当"不可恢复故障"
/// 处理——一个 80-step 任务里偶发 3 次卡就直接 failed，违背 retry 的本意。
/// max_steps 自身（80）已是 retry 总次数的隐式上限，加上 cancel_token + UI 可见
/// 的 system_hint，无需再加任务级 budget。
const DEFAULT_IDLE_RETRY_BUDGET: u32 = 2;

/// Single-Agent Uplift P1-3: 撞顶 (`stop_reason == "length"`) 时升档使用的 max_tokens。
///
/// **第一档恢复**：默认 `max_output_tokens`（16K）撞顶时，**同 step 内**升档到 64K
/// 重发。覆盖 80% 撞顶场景——大多数撞顶其实只略超 8-16K。
///
/// 选 64K 是因为：Anthropic Claude 4 / OpenAI o-series / DeepSeek-V4 都支持
/// ≥64K output；更小窗口模型（如 deepseek-coder-32k）由 `compute_escalated_cap`
/// 在 caller 端 clamp 到 `context_window / 2`，避免无谓重试。
pub(crate) const ESCALATED_MAX_OUTPUT_TOKENS: u32 = 65_536;

/// Single-Agent Uplift P1-3: multi-turn "continue from where you cut off" 恢复上限。
///
/// **第二档恢复**：64K 升档后仍撞顶 → 注入 "Resume directly" user message 让 LLM
/// 接着写，最多 3 次。第 4 次还撞顶 → surface error（任务本身太大，需要人介入）。
///
/// 3 次 = Claude Code `MAX_OUTPUT_TOKENS_RECOVERY_LIMIT` 直接 port。在生产数据上够用，
/// 后续按 Miragenty 实测再调。
pub(crate) const MAX_OUTPUT_TOKENS_RECOVERY_LIMIT: u32 = 3;

/// P1-3 升档值的 clamp 计算：不超过模型 context_window / 2，且不低于 caller 当前值。
///
/// 为什么不超过 ctx/2：升档输出占去一半 ctx 就没空间给后续 user 输入 + tool_result。
/// 为什么不低于当前值：clamp 之后还比当前低就直接跳过升档（caller 应判定后走 multi-turn）。
pub(crate) fn compute_escalated_cap(provider: &str, model: &str, current: u32) -> u32 {
    let caps = crate::llm::registry::get_capabilities(provider, model);
    let upper = (caps.context_window / 2) as u32;
    upper.min(ESCALATED_MAX_OUTPUT_TOKENS).max(current)
}

/// 每步 LLM 请求的 max_tokens 默认值。
/// 详细动机参见 [`AgentRunOptions::max_output_tokens`]。
pub const DEFAULT_AGENT_MAX_OUTPUT_TOKENS: u32 = 16_384;

/// Issue 3: 纯函数版的"idle-retry budget 转移"语义。
///
/// 抽出来仅是为了写单测——loop 里实际还是 inline 状态机。**契约**：
/// - 进入新 step（`resume_after_idle_retry == false`）→ budget 重置到 `default`
/// - 上一次是 retry 跳过来的（`resume_after_idle_retry == true`）→ 保留当前 budget
///
/// 任何对这个函数的"简化"（例如忘了 reset 或永远 reset）都会被下面 mod tests 抓住。
#[inline]
fn next_idle_retry_budget(resume_after_idle_retry: bool, current: u32, default: u32) -> u32 {
    if resume_after_idle_retry {
        current
    } else {
        default
    }
}

/// 只读工具集合（不会改变工作区状态）。L3 循环检测据此判断是否在原地探索。
///
/// 同时识别 `grep`（主名，2026-05 起）与 `search_files`（alias，旧 session replay
/// 兼容）。如果将来加新只读工具，直接列举即可。
fn is_read_only_tool(name: &str) -> bool {
    crate::tools::lookup_tool_spec(name)
        .map(|spec| spec.is_read_only)
        .unwrap_or(false)
}

fn tool_is_read_only_loop_exploration(name: &str, input: &serde_json::Value) -> bool {
    is_read_only_tool(name)
        || (name == "shell_exec" && shell_command_looks_like_read_only_exploration(input))
}

fn shell_command_looks_like_read_only_exploration(input: &serde_json::Value) -> bool {
    let Some(command) = input.get("command").and_then(|v| v.as_str()) else {
        return false;
    };
    let cmd = command.trim().to_ascii_lowercase();
    if cmd.is_empty() {
        return false;
    }
    if shell_command_has_output_redirection(&cmd) || shell_download_writes_file(&cmd) {
        return false;
    }
    if cmd.contains("python")
        && (cmd.contains("fitz")
            || cmd.contains("pymupdf")
            || cmd.contains("pypdf")
            || cmd.contains("pdfplumber")
            || cmd.contains("pdftotext")
            || cmd.contains("get_text")
            || cmd.contains("find_tables")
            || cmd.contains("arxiv_page.html"))
        && !cmd.contains("presentation()")
        && !cmd.contains("prs.save")
        && !cmd.contains("savefig")
        && !cmd.contains("write_text")
        && !cmd.contains("write_bytes")
    {
        return true;
    }
    let read_only_starters = [
        "curl ", "wget ", "grep ", "rg ", "find ", "ls ", "cat ", "head ", "tail ", "sed -n",
    ];
    let read_only_pipes = [
        "grep ",
        "rg ",
        "head ",
        "tail ",
        "sed -n",
        "python3 -m json.tool",
    ];
    read_only_starters
        .iter()
        .any(|starter| cmd.starts_with(starter))
        || read_only_pipes
            .iter()
            .any(|marker| cmd.contains(&format!("| {marker}")))
}

fn shell_command_has_output_redirection(cmd: &str) -> bool {
    cmd.contains(" >") || cmd.contains(">>") || cmd.contains(" 1>") || cmd.contains("| tee ")
}

fn shell_download_writes_file(cmd: &str) -> bool {
    let is_download = cmd.starts_with("curl ")
        || cmd.starts_with("wget ")
        || cmd.contains(" curl ")
        || cmd.contains(" wget ");
    is_download
        && (cmd.contains(" -o ")
            || cmd.contains(" -O")
            || cmd.contains(" --output ")
            || cmd.contains(" --output=")
            || cmd.contains(" --output-document ")
            || cmd.contains(" --output-document=")
            || shell_command_has_output_redirection(cmd))
}

fn read_only_loop_allows_tool(name: &str, input: &serde_json::Value) -> bool {
    artifact_checkpoint_allows_tool(name, input) || !tool_is_read_only_loop_exploration(name, input)
}

fn artifact_checkpoint_allows_tool(name: &str, input: &serde_json::Value) -> bool {
    if matches!(
        name,
        "write_file" | "edit_file" | "notebook_edit" | "publish_artifact" | "task_complete"
    ) {
        return true;
    }
    name == "shell_exec" && shell_command_looks_like_artifact_work(input)
}

fn urgent_artifact_checkpoint_allows_tool(
    name: &str,
    input: &serde_json::Value,
    missing: &[String],
) -> bool {
    if name == "task_complete" {
        return true;
    }
    if matches!(name, "write_file" | "edit_file" | "notebook_edit") {
        let Some(path) = input
            .get("path")
            .or_else(|| input.get("file_path"))
            .and_then(|v| v.as_str())
        else {
            return false;
        };
        return missing
            .iter()
            .any(|missing_path| path.ends_with(missing_path));
    }
    if name != "shell_exec" || !shell_command_looks_like_artifact_work(input) {
        return false;
    }
    let Some(command) = input.get("command").and_then(|v| v.as_str()) else {
        return false;
    };
    let lower = command.to_ascii_lowercase();
    if missing
        .iter()
        .any(|path| path.to_ascii_lowercase().ends_with(".pptx"))
        && lower.contains("python")
        && (lower.contains("ppt") || lower.contains("presentation"))
        && (lower.contains("<<")
            || lower.contains("create")
            || lower.contains("build")
            || lower.contains("generate")
            || lower.contains("gen_")
            || lower.contains("make"))
    {
        return true;
    }
    missing
        .iter()
        .all(|path| lower.contains(&path.to_ascii_lowercase()))
}

fn artifact_checkpoint_allows_tool_for_remaining_steps(
    name: &str,
    input: &serde_json::Value,
    missing: &[String],
    remaining_steps: u32,
) -> bool {
    if remaining_steps <= 2 && !missing.is_empty() {
        artifact_checkpoint_allows_tool(name, input)
    } else if remaining_steps <= 4 && !missing.is_empty() {
        urgent_artifact_checkpoint_allows_tool(name, input, missing)
    } else {
        artifact_checkpoint_allows_tool(name, input)
    }
}

fn read_only_loop_block_feedback(blocked_tool: &str, missing: &[String]) -> String {
    format!(
        "[System] Tool `{blocked_tool}` was not run because this artifact-producing task is stuck in a read/search loop while required output artifact(s) are still missing or placeholder-only: {}. Stop gathering more evidence. Create or repair the required artifacts now using write_file/edit_file/notebook_edit or a local artifact-generation shell command, then validate and call task_complete.",
        missing.join(", ")
    )
}

fn finalization_allows_tool(name: &str, input: &serde_json::Value) -> bool {
    artifact_checkpoint_allows_tool(name, input)
}

fn direct_output_loop_block_feedback(blocked_tool: &str) -> String {
    format!(
        "[System] Tool `{blocked_tool}` was not run because this direct-response task is stuck in a read/search loop. Stop gathering more evidence now and call task_complete with the requested final answer in task_complete.summary. Do not write files just to hold the final answer."
    )
}

fn has_direct_response_contract(opts: &AgentRunOptions) -> bool {
    opts.task_contract
        .as_ref()
        .map(|contract| contract.requires_direct_response())
        .unwrap_or(false)
        || has_direct_response_guardrail(&opts.guardrails)
}

fn has_direct_response_guardrail(guardrails: &[Guardrail]) -> bool {
    guardrails.iter().any(|guardrail| {
        matches!(
            guardrail,
            Guardrail::SummaryMatches { .. }
                | Guardrail::SummaryJsonValid { .. }
                | Guardrail::SummaryNonEmpty
        )
    })
}

fn shell_command_looks_like_artifact_work(input: &serde_json::Value) -> bool {
    let Some(command) = input.get("command").and_then(|v| v.as_str()) else {
        return false;
    };
    let cmd = command.to_ascii_lowercase();
    if shell_command_has_output_redirection(&cmd) || shell_download_writes_file(&cmd) {
        return true;
    }
    let artifact_terms = [
        ".pptx", ".ipynb", ".json", ".csv", ".md", ".png", ".jpg", ".jpeg", ".pdf", ".html", ".txt",
    ];
    let script_terms = [".py", ".js", ".rb", ".sh"];
    let generator_markers = [
        "heredoc",
        "<<'",
        "<<\"",
        "cat >",
        "cat <<",
        "tee ",
        "build_",
        "generate_",
        "gen_",
        "create_",
        "make_",
    ];
    let local_execution = cmd.contains("python")
        || cmd.contains("node")
        || cmd.contains("ruby")
        || cmd.contains("pandoc")
        || cmd.contains("libreoffice")
        || cmd.contains("jupyter")
        || cmd.contains("zip")
        || cmd.contains("unzip")
        || cmd.contains("test -")
        || cmd.contains("stat ")
        || cmd.contains("file ");
    let is_python_inline = cmd.contains("python -c")
        || cmd.contains("python3 -c")
        || cmd.contains("python -m")
        || cmd.contains("python3 -m");
    let inline_artifact_generation = cmd.contains("python")
        && (cmd.contains("from pptx import")
            || cmd.contains("presentation()")
            || cmd.contains("prs.save")
            || cmd.contains("savefig")
            || cmd.contains("image.new")
            || cmd.contains("zipfile")
            || (artifact_terms.iter().any(|term| cmd.contains(term))
                && (cmd.contains(".write(")
                    || cmd.contains("write_bytes")
                    || cmd.contains("write_text"))));
    if is_python_inline {
        return inline_artifact_generation;
    }
    let artifact_term_present = artifact_terms.iter().any(|term| cmd.contains(term));
    if local_execution && artifact_term_present {
        return true;
    }
    let shell_words = cmd.split_whitespace().collect::<Vec<_>>();
    let executes_script_arg = shell_words.windows(2).any(|pair| {
        matches!(
            pair[0],
            "python" | "python3" | "node" | "ruby" | "bash" | "sh"
        ) && script_terms.iter().any(|ext| pair[1].ends_with(ext))
    });
    let executes_local_script =
        cmd.contains("./") && script_terms.iter().any(|ext| cmd.contains(ext));
    let writes_generator_script = script_terms.iter().any(|ext| cmd.contains(ext))
        && generator_markers.iter().any(|marker| cmd.contains(marker))
        && (cmd.contains('>') || cmd.contains("tee ") || cmd.contains("write_text"));
    if executes_script_arg || executes_local_script || writes_generator_script {
        return true;
    }
    inline_artifact_generation
}

fn artifact_checkpoint_feedback(
    blocked_tool: &str,
    missing: &[String],
    remaining_steps: u32,
) -> String {
    format!(
        "[System] Tool `{blocked_tool}` was not run because only {remaining_steps} step(s) remain and required output artifact(s) are still missing or empty: {}. Stop exploration and do not patch or inspect helper scripts. Create the missing required artifact(s) directly now. Prefer one bounded write_file/edit_file/notebook_edit call, or one local shell heredoc/generator that writes every missing artifact path explicitly, then call task_complete. Partial but well-formed artifacts are better than timing out with no output.",
        missing.join(", ")
    )
}

fn is_required_file_guardrail(guardrail: &Guardrail) -> Option<&[String]> {
    match guardrail {
        Guardrail::FilesNonEmpty { globs } | Guardrail::FilesJsonValid { globs, .. } => Some(globs),
        _ => None,
    }
}

fn required_file_outputs(opts: &AgentRunOptions) -> Vec<String> {
    let mut files = opts
        .task_contract
        .as_ref()
        .map(|contract| contract.required_artifact_paths())
        .unwrap_or_default();
    for guardrail in &opts.guardrails {
        if let Some(globs) = is_required_file_guardrail(guardrail) {
            files.extend(globs.iter().filter(|g| !g.trim().is_empty()).cloned());
        }
    }
    files.sort();
    files.dedup();
    files
}

fn required_files_status(repo_root: &Path, globs: &[String]) -> (Vec<String>, Vec<String>) {
    let mut present = Vec::new();
    let mut missing = Vec::new();
    for g in globs {
        let pattern = repo_root.join(g);
        let pattern_str = pattern.to_string_lossy().to_string();
        let mut any_ready = false;
        if let Ok(entries) = glob::glob(&pattern_str) {
            for entry in entries.flatten() {
                if required_file_entry_is_ready(&entry) {
                    any_ready = true;
                    break;
                }
            }
        }
        if any_ready {
            present.push(g.clone());
        } else {
            missing.push(g.clone());
        }
    }
    (present, missing)
}

fn required_file_entry_is_ready(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() || metadata.len() == 0 {
        return false;
    }
    if metadata.len() > 256 * 1024 {
        return true;
    }
    let Ok(content) = std::fs::read_to_string(path) else {
        return true;
    };
    !looks_like_placeholder_artifact(path, &content)
}

fn looks_like_placeholder_artifact(path: &Path, content: &str) -> bool {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return true;
    }
    let lower = trimmed.to_ascii_lowercase();
    if [
        "todo",
        "tbd",
        "placeholder",
        "待补充",
        "待定",
        "待获取",
        "待下载",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
    {
        return true;
    }
    if path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.eq_ignore_ascii_case("json"))
        .unwrap_or(false)
    {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
            return json_value_looks_placeholder(&value);
        }
    }
    false
}

fn json_value_looks_placeholder(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Object(map) => map.values().any(json_value_looks_placeholder),
        serde_json::Value::Array(values) => values.is_empty(),
        serde_json::Value::String(s) => {
            let trimmed = s.trim();
            trimmed.is_empty()
                || matches!(
                    trimmed.to_ascii_lowercase().as_str(),
                    "todo" | "tbd" | "placeholder" | "unknown" | "n/a"
                )
        }
        serde_json::Value::Null => true,
        _ => false,
    }
}

fn artifact_first_hint(missing: &[String], step: u32) -> String {
    format!(
        "[System] Required output artifact(s) are still missing or empty by step {step}: {}. Create or update these files now with best-effort valid content, then continue gathering evidence only if needed.",
        missing.join(", ")
    )
}

fn timeout_finalization_hint(remaining_secs: u64, missing: &[String]) -> String {
    let missing_text = if missing.is_empty() {
        "all required files appear to exist; finalize and call task_complete".to_string()
    } else {
        format!(
            "these required files are still missing or empty: {}; write best-effort valid artifacts before task_complete",
            missing.join(", ")
        )
    };
    format!(
        "[System] Wall-clock budget is nearly exhausted (~{remaining_secs}s remain). Stop exploration now: {missing_text}."
    )
}

fn finalization_tool_block_feedback(
    blocked_tool: &str,
    remaining_secs: u64,
    missing: &[String],
) -> String {
    let missing_text = if missing.is_empty() {
        "required artifacts appear to exist".to_string()
    } else {
        format!(
            "missing or empty required artifacts: {}",
            missing.join(", ")
        )
    };
    format!(
        "[System] Tool `{blocked_tool}` was not run because the task is in timeout finalization (~{remaining_secs}s remain; {missing_text}). Only write/edit required artifacts, run local artifact-generation or artifact-validation commands, publish them, or call task_complete."
    )
}

fn guardrail_repair_tool_block_feedback(
    blocked_tool: &str,
    last_repair_feedback: Option<&str>,
) -> String {
    let mut out = format!(
        "[System] Tool `{blocked_tool}` was not run because late guardrail repair mode is active. The guardrail failure already identifies a concrete issue; stop exploring and call task_complete with a corrected final answer, or only edit/regenerate/validate required artifacts when files are required."
    );
    if let Some(feedback) = last_repair_feedback.filter(|s| !s.trim().is_empty()) {
        out.push_str("\n\nLast guardrail repair instruction still applies:\n");
        out.push_str(feedback);
    }
    out
}

fn guardrail_repair_instruction(feedback: &str, required_output_files: &[String]) -> String {
    let mut out = String::from(
        "[System][Guardrail Repair] The previous completion failed concrete validation. Do not gather new evidence or broaden the task. Repair only the failing artifact(s), run a local validation for the exact constraint, then call task_complete again.\n",
    );
    if !required_output_files.is_empty() {
        out.push_str("Required artifact(s): ");
        out.push_str(&required_output_files.join(", "));
        out.push_str(".\n");
    }
    let lower = feedback.to_ascii_lowercase();
    if lower.contains("length must be")
        || lower.contains("non-whitespace chars")
        || lower.contains("too short")
        || lower.contains("too long")
    {
        out.push_str("- Length/shape repair: rewrite the named artifact to the requested size range. If it is overlong, condense each section and remove low-priority prose; if it is too short, add concise task-relevant substance. Preserve required headings, keys, columns, and facts.\n");
    }
    if lower.contains("source marker") || lower.contains("preserve exact source marker") {
        out.push_str("- Source-marker repair: preserve all source marker substrings required by the task and every marker named in the feedback. Copy them verbatim into the final answer in the relevant fact strings; do not paraphrase, split, reorder words inside a marker, or fix only the latest missing marker while dropping earlier ones. If the task evidence is still visible in context, prefer exact phrases from that evidence for all facts that may be graded by source markers.\n");
    }
    if lower.contains("final json code block")
        || lower.contains("requested json value")
        || lower.contains("summary must be the final json")
    {
        out.push_str("- Final-response repair: call task_complete with exactly the requested fenced JSON/text block as the summary, not a prose summary of your work.\n");
    }
    if lower.contains("missing required header") || lower.contains("headers") {
        out.push_str("- Header repair: add the missing required heading(s) exactly as named; do not rename or decorate them.\n");
    }
    if lower.contains("header mismatch") || lower.contains("csv") {
        out.push_str("- CSV repair: make the header exactly match the required columns and keep at least one valid data row.\n");
    }
    if lower.contains("missing keys") || lower.contains("json") {
        out.push_str("- JSON repair: update the existing JSON artifact so required keys are present and the file remains parseable.\n");
    }
    if lower.contains("placeholder") || lower.contains("todo") || lower.contains("tbd") {
        out.push_str("- Placeholder repair: replace placeholder text with concrete content from the existing evidence.\n");
    }
    if lower.contains("pptx") || lower.contains("presentation") || lower.contains("slide") {
        out.push_str("- Presentation repair: edit/regenerate the saved presentation and validate the actual saved file, not just the generator source. If the failure mentions the final slide or an open question, put the exact contiguous phrase `开放问题` (or `open question`) on the literal last slide; variants like `开放的问题` do not satisfy strict graders.\n");
    }
    out.push_str("\nExact guardrail feedback:\n");
    out.push_str(feedback);
    out
}

#[derive(Debug, Clone, Default)]
struct InvalidToolArgsRecoveryState {
    malformed_write_file_count: u32,
}

impl InvalidToolArgsRecoveryState {
    fn observe_tool_batch(
        &mut self,
        tool_use_blocks: &[(String, String, serde_json::Value)],
        tool_outputs: &[Option<crate::tools::ToolOutput>],
        missing_required_files: &[String],
    ) -> Option<String> {
        let malformed_write_files = tool_use_blocks
            .iter()
            .filter(|(_, name, input)| {
                name == "write_file" && tool_input_has_arg_parse_error(input)
            })
            .count() as u32;
        if malformed_write_files > 0 {
            self.malformed_write_file_count += malformed_write_files;
            return Some(malformed_write_file_recovery_hint(
                self.malformed_write_file_count,
                missing_required_files,
            ));
        }

        let delivery_succeeded = missing_required_files.is_empty()
            && tool_use_blocks
                .iter()
                .zip(tool_outputs.iter())
                .any(|((_, name, input), output)| {
                    !tool_input_has_arg_parse_error(input)
                        && matches!(
                            name.as_str(),
                            "write_file"
                                | "edit_file"
                                | "notebook_edit"
                                | "shell_exec"
                                | "publish_artifact"
                        )
                        && output.as_ref().map(|out| !out.is_error).unwrap_or(false)
                });
        if delivery_succeeded {
            self.malformed_write_file_count = 0;
        }
        None
    }
}

fn tool_input_has_arg_parse_error(input: &serde_json::Value) -> bool {
    input
        .as_object()
        .and_then(|obj| obj.get(crate::llm::ARG_PARSE_ERROR_KEY))
        .and_then(|v| v.as_str())
        .is_some()
}

fn malformed_write_file_recovery_hint(attempts: u32, missing: &[String]) -> String {
    let missing_text = if missing.is_empty() {
        "required artifacts are not currently known to be missing".to_string()
    } else {
        format!(
            "required artifacts still missing or empty: {}",
            missing.join(", ")
        )
    };
    if attempts <= 1 {
        return format!(
            "[System] The previous write_file call had malformed JSON arguments ({missing_text}). Do not retry a large write_file payload and do not spend remaining steps appending a long script chunk-by-chunk. Next, prefer exactly one local artifact-generation shell_exec command or heredoc that creates and validates the required files. If you cannot do that, write one minimal complete file/generator under ~1200 characters, then run it."
        );
    }
    format!(
        "[System][Recovery Escalation] write_file arguments have been malformed {attempts} times ({missing_text}). Stop attempting large write_file bodies. On the next turn choose exactly one bounded recovery action: (1) run a local artifact-generation shell command or heredoc that creates/validates the required files; (2) write a minimal complete generator/file with content under ~1200 characters; or (3) if valid artifacts already exist, publish them or call task_complete. Do not inspect script lines or retry another large JSON string."
    )
}

fn llm_error_looks_transient_network(msg: &str) -> bool {
    let lower = msg.to_ascii_lowercase();
    lower.contains("error sending request")
        || lower.contains("connection reset")
        || lower.contains("connection closed")
        || lower.contains("connection aborted")
        || lower.contains("operation timed out")
        || lower.contains("deadline has elapsed")
        || lower.contains("dns error")
        || lower.contains("tcp")
        || lower.contains("tls")
}

fn transient_network_retry_delay(attempt: u32, initial_ms: u64) -> std::time::Duration {
    let shift = attempt.saturating_sub(1).min(5);
    let millis = initial_ms.saturating_mul(1u64 << shift).min(16_000);
    std::time::Duration::from_millis(millis.max(250))
}

fn long_task_policy_allows_tool(name: &str, remaining_steps: u32) -> bool {
    if remaining_steps > LONG_TASK_FINALIZATION_STEPS {
        return true;
    }
    !matches!(name, "todo_write" | "enter_plan_mode" | "ask_user_question")
}

fn long_task_policy_feedback(blocked_tool: &str, remaining_steps: u32) -> String {
    format!(
        "[System] This task is in the finalization phase ({remaining_steps} step(s) remain). \
         Do not call `{blocked_tool}` now. Use the evidence already collected, write or validate \
         the required artifacts, then call `task_complete`. Only run a new read/search/shell command \
         if it directly verifies a required output."
    )
}

fn no_tool_progress_hint(consecutive_no_tool: u32, last_response_had_visible_text: bool) -> String {
    if last_response_had_visible_text {
        return format!(
            "[System] You have produced {} replies without using any tool. \
             Either continue with a tool call or signal completion via the \
             `task_complete` tool. The task is NOT considered complete until \
             `task_complete` succeeds.",
            consecutive_no_tool
        );
    }
    "[System] Your previous replies contained no visible answer and no tool call. Produce a visible final answer now if the task asks for direct output, or use an appropriate tool; do not continue with hidden reasoning only.".to_string()
}

fn assistant_text_from_blocks(blocks: &[ContentBlock]) -> Option<String> {
    let text = blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    (!text.trim().is_empty()).then_some(text)
}

fn char_safe_excerpt(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let excerpt = chars.by_ref().take(max_chars).collect::<String>();
    let remaining_chars = chars.count();
    if remaining_chars > 0 {
        format!("{excerpt}…[+{remaining_chars} chars]")
    } else {
        excerpt
    }
}

/// **DeepSeek / OpenAI tool-call 协议适配器：tool_calls 之后唯一合规的 follow-up 回合。**
///
/// # 三家协议共用的硬约束
///
/// 含 `tool_calls`（OpenAI/DeepSeek）或 `tool_use`（Anthropic）的 assistant message
/// 之后**必须紧接**一条 user 回合，且该回合内容须以与之配对的 tool_results 为主。
/// 中间不能塞独立的 `[user text]` message，否则 DeepSeek/OpenAI 服务端会以
/// `insufficient tool messages following tool_calls message` 直接 400 ——
/// 这是确定性错误，stream-retry 5 次都救不回来（已在生产链路 f5866369 复现）。
///
/// # 为何要 builder 而非 inline 拼装
///
/// 早先的 inline 写法把 `read_only_loop_hint` / `queued_notes` / `max_tokens_hit_hint`
/// 等各种"附带的提示文本"分散在主循环里，作者一不留神就会写出 `messages.push(user
/// text)` 然后再 `messages.push(user tool_results)` 的两条独立 message，**编译期
/// 看不出来**。本 builder 把这两类内容收敛进同一条 user `Message`，在类型层把
/// "拆两条" 这条路堵死。
///
/// # OpenAI 协议下的最终序列
///
/// `convert_messages`（[`crate::llm::openai_compat`]）会把这条 Message 解构成：
///
/// ```text
/// [role=tool, tool_call_id=A, content=...]
/// [role=tool, tool_call_id=B, content=...]
/// [role=user, content=hint_text]    ← 仅当 append_hint 被调用过才出现
/// ```
///
/// `tool` messages 一定排在 `user` 文本之前，跟在 `assistant tool_calls` 后面，
/// 完全符合 OpenAI/DeepSeek 协议的"tool_calls → tool_results → 下一回合输入"。
///
/// # Anthropic 协议下
///
/// 直接序列化为单条 `role=user`、内含 ToolResult* + Text 的 message ——
/// Anthropic 协议本身就允许 ToolResult 与 Text 在同一 user message 内混用。
struct ToolFollowupBuilder {
    /// 严格按原 `tool_use_blocks` 顺序的 tool_results。Anthropic 协议要求
    /// 顺序与同 turn 的 ToolUse 一一对应。
    tool_results: Vec<ContentBlock>,
    /// 本回合追加给 LLM 的提示文本片段（按 push 顺序拼接）。最终合并成
    /// 单一 Text block 放在 tool_results 之后。
    hints: Vec<String>,
}

impl ToolFollowupBuilder {
    fn with_capacity(n: usize) -> Self {
        Self {
            tool_results: Vec::with_capacity(n),
            hints: Vec::new(),
        }
    }

    fn push_tool_result(&mut self, tool_use_id: String, content: String, is_error: bool) {
        self.tool_results.push(ContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
        });
    }

    /// 追加一段提示文本到本回合 follow-up message。空串忽略。
    fn append_hint(&mut self, hint: impl Into<String>) {
        let s = hint.into();
        if !s.is_empty() {
            self.hints.push(s);
        }
    }

    /// 构造最终 user Message。**tool_results 总是排在 hint Text 之前**——
    /// 这是 OpenAI 协议合规的关键，convert 时 tool_call_id 的紧邻配对靠它保证。
    fn build(self) -> Message {
        let mut content = self.tool_results;
        if !self.hints.is_empty() {
            content.push(ContentBlock::Text {
                text: self.hints.join("\n\n"),
            });
        }
        Message {
            role: MessageRole::User,
            content,
            cache_control: None,
        }
    }
}

// ---- Single-Agent Uplift Phase 2.2: 上下文瘦身 ----

/// 单个 tool_result 内容超过 8KB chars 就触发截断。
/// 之所以用 chars 而非 bytes：tokenizer 对字符敏感而非字节，且 8KB chars ≈ 2K tokens
/// ——一条 result 占 2K tokens 已经是"开始挤垮 prompt"的临界值。
const TOOL_RESULT_BUDGET_CHARS: usize = 8 * 1024;
/// 截断后保留尾部 N chars——尾部往往是错误堆栈 / 最终输出，比头部更重要。
const TOOL_RESULT_TAIL_CHARS: usize = 1024;
const TOOL_USE_INPUT_BUDGET_CHARS: usize = 2 * 1024;
const TOOL_USE_INPUT_EXCERPT_CHARS: usize = 700;
const TOOL_USE_INPUT_RECENT_MESSAGE_WINDOW: usize = 8;
const TASK_COMPLETE_EVENT_SUMMARY_CHARS: usize = 2048;
/// "已经截过的"哨兵串前缀，避免重复截断把 sentinel 自己当原内容再截一次。
const TRUNCATED_SENTINEL_PREFIX: &str = "[result truncated to keep context lean.";
/// 整个 prompt 的 token 预算（粗估）。超过就 microcompact。
/// 50K 是大多数 chat completion 模型 (Claude / GPT-4o / DeepSeek) 安全区的下沿。
const MICROCOMPACT_TOKEN_THRESHOLD: usize = 50_000;
/// 长轨迹 working-memory 压缩的提前触发阈值。它不等 prompt 撞到 50K，
/// 而是在工具调用/消息数量已经显示出“会反复 replay”时先折叠早期轨迹。
const WORKING_MEMORY_MESSAGE_THRESHOLD: usize = 18;
const WORKING_MEMORY_TOOL_BLOCK_THRESHOLD: usize = 14;
const WORKING_MEMORY_TOKEN_THRESHOLD: usize = 24_000;
const WORKING_MEMORY_MIN_REMAINING_MESSAGES: usize = 8;
/// 长任务后段不再允许重新规划类工具；此时应该收敛到产物验证和 task_complete。
const LONG_TASK_FINALIZATION_STEPS: u32 = 8;
/// chars-to-tokens 粗估常数。代码 / JSON 大约 3.5 chars/token，取 4 偏保守
/// (略高估 → 提前触发压缩，宁可早一步也别超 ctx)。
const CHARS_PER_TOKEN_ESTIMATE: usize = 4;

/// 把 messages 里所有 ToolResult 的 content 截短到 TOOL_RESULT_BUDGET_CHARS 以内。
/// 对已经截过的 result 幂等（看 sentinel 前缀）。
///
/// 为什么 in-place 改：之前以为前端展开按钮要看原文，所以不能动 messages。
/// 实际原文已经在 agent_events.content 里持久化（emit_event_with_meta 走的是原 ToolOutput.content），
/// 改 messages 只影响下次 LLM 请求 —— 用户视角无感。
fn apply_tool_result_budget(messages: &mut [Message]) {
    for msg in messages.iter_mut() {
        for block in msg.content.iter_mut() {
            if let ContentBlock::ToolResult { content, .. } = block {
                if crate::agent::tool_result_policy::is_already_compacted(content) {
                    continue;
                }
                let total = content.chars().count();
                if total <= TOOL_RESULT_BUDGET_CHARS {
                    continue;
                }
                let tail: String = content
                    .chars()
                    .skip(total - TOOL_RESULT_TAIL_CHARS)
                    .collect();
                let total_kb = total / 1024;
                *content = format!(
                    "{TRUNCATED_SENTINEL_PREFIX} Original size: {total_kb}KB. Last {} chars:\n{tail}]",
                    TOOL_RESULT_TAIL_CHARS,
                );
            }
        }
    }
}

fn collect_completed_tool_use_ids(messages: &[Message]) -> std::collections::HashSet<String> {
    let mut ids = std::collections::HashSet::new();
    for msg in messages {
        for block in &msg.content {
            if let ContentBlock::ToolResult { tool_use_id, .. } = block {
                ids.insert(tool_use_id.clone());
            }
        }
    }
    ids
}

fn compact_input_hash(input: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    input.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn compact_large_tool_use_inputs(messages: &mut [Message]) -> usize {
    let completed = collect_completed_tool_use_ids(messages);
    let mut compacted = 0usize;
    let compact_until = messages
        .len()
        .saturating_sub(TOOL_USE_INPUT_RECENT_MESSAGE_WINDOW);
    for msg in messages.iter_mut().take(compact_until) {
        if !matches!(msg.role, MessageRole::Assistant) {
            continue;
        }
        for block in msg.content.iter_mut() {
            let ContentBlock::ToolUse { id, name, input } = block else {
                continue;
            };
            if !completed.contains(id) {
                continue;
            }
            if input
                .get("__tool_use_input_compacted__")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                continue;
            }
            let Ok(raw) = serde_json::to_string(input) else {
                continue;
            };
            if raw.chars().count() <= TOOL_USE_INPUT_BUDGET_CHARS {
                continue;
            }
            let hash = compact_input_hash(&raw);
            let excerpt = char_safe_excerpt(&raw, TOOL_USE_INPUT_EXCERPT_CHARS);
            let mut replacement = serde_json::json!({
                "__tool_use_input_compacted__": true,
                "__non_executable_history_stub__": true,
                "note": "Historical completed tool_use arguments were compacted for context only. Do not copy this object into a new tool call; re-create valid arguments explicitly.",
                "original_chars": raw.chars().count(),
                "hash16": hash,
                "__tool_use_input_excerpt__": excerpt,
            });
            if let Some(path) = input
                .get("path")
                .or_else(|| input.get("file_path"))
                .and_then(|v| v.as_str())
            {
                replacement["original_path"] = serde_json::Value::String(path.to_string());
            }
            if let Some(command) = input.get("command").and_then(|v| v.as_str()) {
                replacement["__tool_use_command_excerpt__"] =
                    serde_json::Value::String(char_safe_excerpt(command, 240));
            }
            replacement["tool"] = serde_json::Value::String(name.clone());
            *input = replacement;
            compacted += 1;
        }
    }
    compacted
}

fn approximate_tokens(messages: &[Message]) -> usize {
    let mut chars = 0usize;
    for msg in messages {
        for block in &msg.content {
            chars += match block {
                ContentBlock::Text { text } => text.len(),
                ContentBlock::Reasoning { text } => text.len(),
                ContentBlock::ToolUse { input, .. } => {
                    serde_json::to_string(input).map(|s| s.len()).unwrap_or(0)
                }
                ContentBlock::ToolResult { content, .. } => content.len(),
            };
        }
    }
    chars / CHARS_PER_TOKEN_ESTIMATE
}

fn compact_prefix_preserves_tool_pairing(messages: &[Message], boundary: usize) -> bool {
    if boundary == 0 || boundary >= messages.len() {
        return true;
    }

    let mut dropped_tool_use_ids = std::collections::HashSet::new();
    for msg in &messages[..boundary] {
        for block in &msg.content {
            if let ContentBlock::ToolUse { id, .. } = block {
                dropped_tool_use_ids.insert(id.as_str());
            }
        }
    }
    if dropped_tool_use_ids.is_empty() {
        return true;
    }

    for msg in &messages[boundary..] {
        for block in &msg.content {
            if let ContentBlock::ToolResult { tool_use_id, .. } = block {
                if dropped_tool_use_ids.contains(tool_use_id.as_str()) {
                    return false;
                }
            }
        }
    }
    true
}

fn pairing_safe_compact_prefix_len(
    messages: &[Message],
    desired_drop_count: usize,
    min_remaining: usize,
) -> Option<usize> {
    if desired_drop_count == 0 || messages.len() <= min_remaining {
        return None;
    }
    let max_drop = messages.len().saturating_sub(min_remaining);
    let desired = desired_drop_count.min(max_drop);

    for boundary in desired..=max_drop {
        if compact_prefix_preserves_tool_pairing(messages, boundary) {
            return Some(boundary);
        }
    }

    (1..desired)
        .rev()
        .find(|boundary| compact_prefix_preserves_tool_pairing(messages, *boundary))
}

#[derive(Debug, Clone, Default, serde::Serialize)]
struct ContextStats {
    message_count: usize,
    block_count: usize,
    user_messages: usize,
    assistant_messages: usize,
    text_blocks: usize,
    reasoning_blocks: usize,
    tool_use_blocks: usize,
    tool_result_blocks: usize,
    tool_result_error_blocks: usize,
    text_chars: usize,
    reasoning_chars: usize,
    tool_use_json_chars: usize,
    tool_result_chars: usize,
    largest_tool_result_chars: usize,
    approx_tokens: usize,
}

fn collect_context_stats(messages: &[Message]) -> ContextStats {
    let mut stats = ContextStats {
        message_count: messages.len(),
        approx_tokens: approximate_tokens(messages),
        ..Default::default()
    };
    for msg in messages {
        match msg.role {
            MessageRole::User => stats.user_messages += 1,
            MessageRole::Assistant => stats.assistant_messages += 1,
        }
        for block in &msg.content {
            stats.block_count += 1;
            match block {
                ContentBlock::Text { text } => {
                    stats.text_blocks += 1;
                    stats.text_chars += text.chars().count();
                }
                ContentBlock::Reasoning { text } => {
                    stats.reasoning_blocks += 1;
                    stats.reasoning_chars += text.chars().count();
                }
                ContentBlock::ToolUse { input, .. } => {
                    stats.tool_use_blocks += 1;
                    stats.tool_use_json_chars += serde_json::to_string(input)
                        .map(|s| s.chars().count())
                        .unwrap_or(0);
                }
                ContentBlock::ToolResult {
                    content, is_error, ..
                } => {
                    stats.tool_result_blocks += 1;
                    if *is_error {
                        stats.tool_result_error_blocks += 1;
                    }
                    let chars = content.chars().count();
                    stats.tool_result_chars += chars;
                    stats.largest_tool_result_chars = stats.largest_tool_result_chars.max(chars);
                }
            }
        }
    }
    stats
}

#[derive(Debug, Clone)]
pub(crate) struct CompactReport {
    dropped_messages: usize,
    tools_seen: Vec<String>,
    tokens_before: usize,
    tokens_after: usize,
}

impl CompactReport {
    pub(crate) fn human_readable(&self) -> String {
        format!(
            "Compacted {} earlier message(s) to free context. ~{}K → ~{}K tokens. \
             Earlier tool calls: {}.",
            self.dropped_messages,
            self.tokens_before / 1000,
            self.tokens_after / 1000,
            if self.tools_seen.is_empty() {
                "(none)".to_string()
            } else {
                self.tools_seen.join(", ")
            },
        )
    }

    pub(crate) fn to_meta(&self) -> serde_json::Value {
        serde_json::json!({
            "dropped_messages": self.dropped_messages,
            "tokens_before": self.tokens_before,
            "tokens_after": self.tokens_after,
            "tools_seen": self.tools_seen,
        })
    }
}

fn compacted_tools_seen(messages: &[Message]) -> Vec<String> {
    let mut tools_seen = Vec::new();
    for msg in messages {
        for block in &msg.content {
            if let ContentBlock::ToolUse { name, .. } = block {
                if !tools_seen.contains(name) {
                    tools_seen.push(name.clone());
                }
            }
        }
    }
    tools_seen
}

fn extract_working_memory_refs(messages: &[Message]) -> Vec<String> {
    let mut refs = Vec::new();
    for msg in messages {
        for block in &msg.content {
            let text = match block {
                ContentBlock::Text { text } | ContentBlock::Reasoning { text } => text.as_str(),
                ContentBlock::ToolResult { content, .. } => content.as_str(),
                ContentBlock::ToolUse { input, .. } => {
                    if let Some(path) = input.get("path").and_then(|v| v.as_str()) {
                        if !refs.iter().any(|r| r == path) {
                            refs.push(path.to_string());
                        }
                    }
                    if let Some(paths) = input.get("file_paths").and_then(|v| v.as_array()) {
                        for path in paths.iter().filter_map(|v| v.as_str()) {
                            if !refs.iter().any(|r| r == path) {
                                refs.push(path.to_string());
                            }
                        }
                    }
                    continue;
                }
            };
            for token in text.split(|c: char| {
                c.is_whitespace() || matches!(c, ',' | ')' | '(' | '"' | '\'' | '`' | '[' | ']')
            }) {
                let cleaned = token.trim_end_matches(|c: char| matches!(c, ':' | ';' | '.'));
                if cleaned.len() < 3 || cleaned.len() > 180 {
                    continue;
                }
                let looks_like_ref = cleaned.contains(".miragenty/evidence/")
                    || cleaned.contains('/')
                    || cleaned.ends_with(".json")
                    || cleaned.ends_with(".md")
                    || cleaned.ends_with(".csv")
                    || cleaned.ends_with(".txt")
                    || cleaned.ends_with(".ipynb")
                    || cleaned.ends_with(".py")
                    || cleaned.ends_with(".sql");
                if looks_like_ref && !refs.iter().any(|r| r == cleaned) {
                    refs.push(cleaned.to_string());
                }
                if refs.len() >= 12 {
                    return refs;
                }
            }
        }
    }
    refs
}

fn extract_compact_observation_excerpts(messages: &[Message]) -> Vec<String> {
    let mut excerpts = Vec::new();
    for msg in messages {
        for block in &msg.content {
            let ContentBlock::ToolResult {
                content, is_error, ..
            } = block
            else {
                continue;
            };
            if *is_error || crate::agent::tool_result_policy::is_already_compacted(content) {
                continue;
            }
            let chars = content.chars().count();
            if !(120..=1200).contains(&chars) {
                continue;
            }
            let trimmed = content.trim();
            if trimmed.is_empty()
                || trimmed.starts_with('{')
                || trimmed.starts_with("total ")
                || trimmed.contains("file_unchanged_since_last_read")
            {
                continue;
            }
            excerpts.push(char_safe_excerpt(trimmed, 900));
            if excerpts.len() >= 2 {
                return excerpts;
            }
        }
    }
    excerpts
}

fn render_compact_summary(
    prefix: &str,
    dropped_count: usize,
    dropped: &[Message],
) -> (String, Vec<String>) {
    let tools_seen = compacted_tools_seen(dropped);
    let refs = extract_working_memory_refs(dropped);
    let observation_excerpts = extract_compact_observation_excerpts(dropped);
    let mut summary = format!(
        "{prefix} {dropped_count} earlier message(s) were compacted. Full raw history remains in the workspace timeline. Earlier tools: {}.",
        if tools_seen.is_empty() {
            "(none)".to_string()
        } else {
            tools_seen.join(", ")
        }
    );
    if !observation_excerpts.is_empty() {
        summary.push_str(" Key compacted observations: ");
        summary.push_str(&observation_excerpts.join(" || "));
        summary.push('.');
    }
    if !refs.is_empty() {
        summary.push_str(" Key files/evidence already touched: ");
        summary.push_str(&refs.join(", "));
        summary.push('.');
    }
    summary.push_str(" Do not repeat broad discovery already done; continue from the latest messages and retrieve specific evidence only when needed.");
    (summary, tools_seen)
}

fn should_working_memory_compact(messages: &[Message]) -> bool {
    if messages.len() < WORKING_MEMORY_MESSAGE_THRESHOLD {
        return false;
    }
    let stats = collect_context_stats(messages);
    stats.approx_tokens >= WORKING_MEMORY_TOKEN_THRESHOLD
        || stats.tool_use_blocks >= WORKING_MEMORY_TOOL_BLOCK_THRESHOLD
        || stats.tool_result_blocks >= WORKING_MEMORY_TOOL_BLOCK_THRESHOLD
}

fn working_memory_compact(messages: &mut Vec<Message>) -> Option<CompactReport> {
    if !should_working_memory_compact(messages) {
        return None;
    }
    let before = approximate_tokens(messages);
    let desired = messages
        .len()
        .saturating_sub(WORKING_MEMORY_MIN_REMAINING_MESSAGES);
    let desired = desired.min(messages.len() / 2).max(messages.len() / 3);
    let Some(drop_count) =
        pairing_safe_compact_prefix_len(messages, desired, WORKING_MEMORY_MIN_REMAINING_MESSAGES)
    else {
        return None;
    };
    let dropped: Vec<Message> = messages.drain(0..drop_count).collect();
    let (summary, tools_seen) = render_compact_summary("[working-memory]", drop_count, &dropped);
    messages.insert(
        0,
        Message {
            role: MessageRole::User,
            content: vec![ContentBlock::Text { text: summary }],
            cache_control: None,
        },
    );
    let after = approximate_tokens(messages);
    Some(CompactReport {
        dropped_messages: drop_count,
        tools_seen,
        tokens_before: before,
        tokens_after: after,
    })
}

/// - 不动 system prompt（caller 把它放在 LlmRequest::system，不在 messages 里）
/// - 不动最近 ⅔ messages —— 通常 LLM 当前正在看的上下文都在尾部，截尾就直接退化
/// - 折叠出来的摘要插在最前面（user role）—— LLM 看到一段历史综述比直接断片更不会乱
/// - 只在 messages 数 ≥ 8 时才动手，太少消息折叠收益小且容易丢上下文
/// - 返回 `None` 表示什么也没做（caller 别 emit `compact` 事件）
fn microcompact(messages: &mut Vec<Message>) -> Option<CompactReport> {
    if messages.len() < 8 {
        return None;
    }
    let before = approximate_tokens(messages);
    let Some(drop_count) = pairing_safe_compact_prefix_len(messages, messages.len() / 3, 2) else {
        return None;
    };
    let dropped: Vec<Message> = messages.drain(0..drop_count).collect();

    // 从被丢的 messages 里抽出工具名做 summary —— "你之前用了哪些工具" 比 "你之前讲了啥"
    // 更能帮 LLM 判断"还需不需要重做某事"。
    let mut tools_seen: Vec<String> = Vec::new();
    for msg in &dropped {
        for block in &msg.content {
            if let ContentBlock::ToolUse { name, .. } = block {
                if !tools_seen.contains(name) {
                    tools_seen.push(name.clone());
                }
            }
        }
    }

    let summary = format!(
        "[context-compact] {} earlier message(s) have been compacted to keep the prompt small. \
         The full event history is still visible to the user in the workspace timeline. \
         Tools you ran earlier: {}. Continue from the latest user/tool messages below.",
        drop_count,
        if tools_seen.is_empty() {
            "(none)".to_string()
        } else {
            tools_seen.join(", ")
        },
    );

    messages.insert(
        0,
        Message {
            role: MessageRole::User,
            content: vec![ContentBlock::Text { text: summary }],
            cache_control: None,
        },
    );

    let after = approximate_tokens(messages);
    Some(CompactReport {
        dropped_messages: drop_count,
        tools_seen,
        tokens_before: before,
        tokens_after: after,
    })
}

/// Single-Agent Uplift P0-1: reactive 版的 microcompact。
///
/// 触发场景：LLM API 实际返回 `context_length_exceeded` / `prompt is too long` 等
/// "已经撞墙" 错误后，由 [`run_inner`] 主循环识别（见
/// [`crate::llm::classify_llm_error`]）后调用，**同 step 内**做一次激进压缩然后重发。
///
/// 与 [`microcompact`] 的差异：
///
/// | 维度            | microcompact (proactive)         | reactive_compact_aggressive            |
/// |-----------------|----------------------------------|----------------------------------------|
/// | 触发时机        | 每 step 开头基于本地 token 估算  | API 端真返回拒绝错误后                 |
/// | 最小消息数门槛  | 8                                | 4（已经撞墙，压少胜过 fail）           |
/// | drop 比例       | `len() / 3`                      | `len() / 2`                            |
/// | 摘要文案        | 中性 "earlier explored"          | 显式 "API rejected as too long"        |
/// | 单 step 触发次数| 每 step 最多 1 次（自然）        | 每 step 最多 1 次（caller flag 守住）  |
///
/// **返回 None 的语义**：已经无能为力（messages < 4 或压完只剩 system 在撑），
/// 此时 caller 应让原错误真 bail。这是合同里"反正你试过了，别死循环"的兜底。
pub(crate) fn reactive_compact_aggressive(messages: &mut Vec<Message>) -> Option<CompactReport> {
    // 至少留 2 条消息（一对 user/assistant）给 LLM 当上下文。messages < 4 时压完只剩 1 条
    // assistant 或 1 条 user，毫无意义。
    if messages.len() < 4 {
        return None;
    }
    let before = approximate_tokens(messages);
    let Some(drop_count) = pairing_safe_compact_prefix_len(messages, messages.len() / 2, 2) else {
        return None;
    };
    let dropped: Vec<Message> = messages.drain(0..drop_count).collect();

    let mut tools_seen: Vec<String> = Vec::new();
    for msg in &dropped {
        for block in &msg.content {
            if let ContentBlock::ToolUse { name, .. } = block {
                if !tools_seen.contains(name) {
                    tools_seen.push(name.clone());
                }
            }
        }
    }

    let summary = format!(
        "[context-compact:reactive] The LLM API rejected the previous request as too long. \
         {} earlier message(s) have been aggressively compacted to free space and the request \
         is being retried with the same step. The full event history remains visible to the user \
         in the workspace timeline. Tools you ran earlier: {}. Continue from the latest \
         user/tool messages below.",
        drop_count,
        if tools_seen.is_empty() {
            "(none)".to_string()
        } else {
            tools_seen.join(", ")
        },
    );

    messages.insert(
        0,
        Message {
            role: MessageRole::User,
            content: vec![ContentBlock::Text { text: summary }],
            cache_control: None,
        },
    );

    let after = approximate_tokens(messages);
    Some(CompactReport {
        dropped_messages: drop_count,
        tools_seen,
        tokens_before: before,
        tokens_after: after,
    })
}

fn plan_contains_tool_markup(plan: &str) -> bool {
    let lower = plan.to_ascii_lowercase();
    lower.contains("<｜｜dsml｜｜tool_calls>")
        || lower.contains("<tool_use")
        || lower.contains("<tool_call")
        || lower.contains("invoke name=\"")
        || lower.contains("</invoke>")
}

fn shell_command_invokes_nested_agent(input: &serde_json::Value) -> bool {
    let cmd = input
        .get("cmd")
        .or_else(|| input.get("command"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if cmd.trim().is_empty() {
        return false;
    }
    let invokes_claude_cli = cmd.contains("claude -p")
        || cmd.contains("claude --print")
        || cmd.contains("claude-code")
        || cmd.contains("npx claude")
        || cmd.contains("bunx claude");
    let invokes_generic_agent = cmd.contains("subagent") || cmd.contains("agent run");
    invokes_claude_cli || invokes_generic_agent
}

fn nested_agent_feedback() -> String {
    "[System] Do not spawn a nested coding agent or Claude CLI from shell_exec. Use the current agent's tools directly: read source files, inspect evidence, update artifacts if needed, and call task_complete when the task is ready.".to_string()
}

#[derive(Debug, Clone, serde::Serialize)]
struct AgentEventPayload {
    agent_id: String,
    step: u32,
    kind: String,
    content: String,
    /// Single-Agent Uplift Phase 0.2: 结构化 payload。前端按 kind 解析渲染（diff、todo
    /// 列表、guardrail report）。`None` 表示纯文本事件，前端走 fallback 行为。
    #[serde(skip_serializing_if = "Option::is_none")]
    meta: Option<serde_json::Value>,
}

#[async_trait::async_trait]
pub trait CustomToolHandler: Send + Sync {
    fn handles_tool(&self, name: &str) -> bool;

    async fn execute_tool(&self, name: &str, input: &serde_json::Value)
        -> crate::tools::ToolOutput;
}

/// AgentEngine 运行时配置（FR-09 / FR-11）。
pub struct AgentRunOptions {
    pub model: String,
    pub max_steps: u32,
    pub timeout_secs: u64,
    pub guardrails: Vec<Guardrail>,
    pub task_contract: Option<TaskContract>,
    pub guardrail_retry_budget: u32,
    /// 来自 task.produces_artifacts 解析后的 (local_name, type) 对，供 ArtifactsExist guardrail 使用。
    pub produces: Vec<(String, String)>,
    pub expected_output: Option<String>,
    /// Issue 3: stream idle timeout 时给 LLM 发"continue"重试的次数预算。
    pub idle_retry_budget: u32,
    /// 每步 LLM 请求的 `max_tokens`。从 AppConfig.agent_max_output_tokens 读取；
    /// 旧实现固定 4096 → 一次生成大文档时 tool_use args 被截断为非法 JSON。
    pub max_output_tokens: u32,
    /// stream "未收到首 chunk" 时网络错误的指数退避重试次数。
    /// 0 = 不重试；3 = 1s/2s/4s 三次。
    pub stream_network_retries: u32,
    /// 网络重试首次退避毫秒数。
    pub stream_initial_retry_delay_ms: u64,
    /// Single-Agent Uplift P0-2: 单 agent 整个任务的 output_token 软上限。
    ///
    /// `Some(n)` → 启用 [`budget_tracker::BudgetTracker`]：累计 output ≥ 90% 或者
    /// 连续 3 轮 delta < 500 token 时往 conversation 注入一条"该收尾了"的提示，
    /// 让 agent 自己调 task_complete。`max_steps` 仍是硬上限兜底。
    ///
    /// `None` → 关闭，行为同 P0-2 之前（只走 `max_steps` 硬上限）。
    ///
    /// 推荐值：模型 `context_window × 30%`。30% 来自经验——output 通常是 input 的
    /// 1/3 到 1/5，task 完成时 output 占 ctx 30% 已是"高消耗"。计算放在 caller
    /// （commands/scheduler），engine 只负责执行。
    pub output_token_budget: Option<u64>,
    /// Single-Agent Uplift P1-2: 可选的备用模型。
    ///
    /// `Some(name)` → 主模型遇到 Overloaded/RateLimited 时切到该模型重发当前 step
    /// （本 step 只切一次，避免环路）。`None` → 关闭 fallback，行为同 P1-2 之前
    /// （overload → bail）。
    ///
    /// **跨 provider fallback 暂不支持**：fallback 模型必须能被同一 provider 调起
    /// （即 OpenAI-compat 内部不同模型，或 Anthropic 内部不同模型）。跨厂商切换
    /// 会触发 tool schema / cache control 不兼容，单独 PR 处理。
    pub fallback_model: Option<String>,
    /// Single-Agent Uplift P1-2: fallback 触发后的粘性策略。
    ///
    /// `true`（默认）→ 切到 fallback 后后续 step 继续用 fallback，直到 agent 结束。
    /// 原因：reseller overload 通常持续 5-30 分钟，频繁回切 primary 等于反复撞墙。
    ///
    /// `false` → 下一个 step 重新尝试 primary。当用户希望"只是临时切一下"时用，
    /// 但要意识到大概率立刻再次撞 overload。
    pub fallback_sticky: bool,
    /// Benchmark-only extension point. Defaults empty so normal Miragenty and
    /// existing benchmarks see the exact default coding tool list.
    pub extra_tools: Vec<crate::llm::ToolDefinition>,
}

impl Default for AgentRunOptions {
    fn default() -> Self {
        Self {
            model: String::new(),
            max_steps: DEFAULT_MAX_AGENT_STEPS,
            timeout_secs: DEFAULT_AGENT_TIMEOUT_SECS,
            guardrails: Vec::new(),
            task_contract: None,
            guardrail_retry_budget: 3,
            produces: Vec::new(),
            expected_output: None,
            idle_retry_budget: DEFAULT_IDLE_RETRY_BUDGET,
            max_output_tokens: DEFAULT_AGENT_MAX_OUTPUT_TOKENS,
            stream_network_retries: 5,
            stream_initial_retry_delay_ms: 1000,
            // P0-2: 默认关闭，需 caller (scheduler / commands::agent::run) 显式启用。
            // 这保证 P0-2 PR 完全向后兼容——旧调用点 Default::default() 还是走 max_steps。
            output_token_budget: None,
            // P1-2: 默认关闭——大部分用户没配 fallback model。caller 显式注入 Some(name) 才启用。
            fallback_model: None,
            // P1-2: 启用 fallback 时默认 sticky=true（见字段文档）
            fallback_sticky: true,
            extra_tools: Vec::new(),
        }
    }
}

/// P2-1 Phase B：hook 调用的 fatal 信号。详见 [`AgentEngine::dispatch_hook_phase`] 文档。
#[derive(Debug, Clone)]
enum HookFatal {
    /// 整 agent 立即 failed。原因字串将被填入 events.error 给前端展示。
    Terminal(String),
    /// 当前 step 提前结束（不再调 LLM / 不再执行 tool），agent 仍存活。
    StepAborted(String),
}

pub struct AgentEngine {
    provider: Arc<dyn LlmProvider>,
    tool_executor: ToolExecutor,
    workspace_root: PathBuf,
    app_handle: tauri::AppHandle,
    cancel_token: CancellationToken,
    /// Single-Agent Uplift B2: 可选的小模型 summarizer。
    /// None 时 apply_tool_result_budget 退化到旧版"截尾保留 1KB"行为。
    tool_summarizer: Option<crate::agent::tool_summarizer::ToolSummarizer>,
    /// 摘要触发阈值（chars）。超过此值的 tool_result 才走摘要路径，
    /// 减少没意义的小请求往返。
    tool_summary_threshold_chars: usize,
    /// Single-Agent Uplift P2-1 Phase B：通用 hook registry。
    ///
    /// 默认空（行为同旧版）。caller 用 [`with_hooks`] 注入：典型链路为
    /// builtin（GuardrailHook 等）+ workspace `.miragenty/hooks.json`
    /// （Phase C 落地）。engine 在 7 个 [`HookPhase`] 调用点 fan-out 给 registry。
    ///
    /// 用 [`Arc`] 是因为 registry 在多个 phase 调用点共享只读访问，且 hooks 内部
    /// 可能持有 Send + Sync 资源（线程池 / DB 连接池等），Arc 让 engine 自身
    /// 不持有 phase 调用的可变借用。
    hook_registry: Arc<crate::agent::hooks::HookRegistry>,
    custom_tool_handler: Option<Arc<dyn CustomToolHandler>>,
}

impl AgentEngine {
    pub fn new(
        provider: Arc<dyn LlmProvider>,
        workspace_root: PathBuf,
        app_handle: tauri::AppHandle,
        cancel_token: CancellationToken,
    ) -> Self {
        Self {
            provider,
            tool_executor: ToolExecutor::new(workspace_root.clone())
                .with_rg_resource_dir(app_handle.path().resource_dir().ok())
                .with_cancel_token(cancel_token.clone()),
            workspace_root,
            app_handle,
            cancel_token,
            tool_summarizer: None,
            tool_summary_threshold_chars: TOOL_RESULT_BUDGET_CHARS,
            hook_registry: Arc::new(crate::agent::hooks::HookRegistry::new()),
            custom_tool_handler: None,
        }
    }

    /// Single-Agent Uplift B2: 注入 tool_summary 小模型。
    /// 由 caller 从 ConfigManager 读 tool_summary_* 配置 + api_key 自行构造，
    /// 这里只负责"挂上来"。caller 不传 = 关闭摘要。
    pub fn with_tool_summarizer(
        mut self,
        summarizer: crate::agent::tool_summarizer::ToolSummarizer,
        threshold_chars: usize,
    ) -> Self {
        self.tool_summarizer = Some(summarizer);
        self.tool_summary_threshold_chars = threshold_chars.max(TOOL_RESULT_BUDGET_CHARS);
        self
    }

    /// Single-Agent Uplift P2-1 Phase B：注入 hook registry。
    ///
    /// Caller 构造好包含 builtin + workspace + user-global 的 registry 后传进来。
    /// 不调此方法 = 空 registry = 所有 phase 调用点 `Pass`，行为完全等同旧版。
    ///
    /// 设计上接受 owned `HookRegistry` 然后 `Arc::new`：避免 caller 关心 Arc，
    /// 也避免在 multi-engine 场景（极少）误把 registry 同步给意料外的 agent。
    pub fn with_hooks(mut self, registry: crate::agent::hooks::HookRegistry) -> Self {
        self.hook_registry = Arc::new(registry);
        self
    }

    pub fn with_custom_tool_handler(mut self, handler: Arc<dyn CustomToolHandler>) -> Self {
        self.custom_tool_handler = Some(handler);
        self
    }

    /// 兼容旧调用点：保留旧签名（max_steps），其它走 Default。
    /// FM-15 Phase 3 后续应迁移到 `run_with_options`。
    pub async fn run(
        &self,
        agent_id: &str,
        task_description: &str,
        model: &str,
        max_steps: u32,
    ) -> Result<AgentStatus> {
        // 兼容入口：尝试从 ConfigManager 读相关字段；没有 ConfigManager 时（单测）落 Default。
        let (max_output_tokens, stream_retries, stream_delay) = self
            .app_handle
            .try_state::<crate::commands::ConfigManager>()
            .map(|m| {
                let c = m.get_config_snapshot();
                (
                    c.agent_max_output_tokens,
                    c.stream_network_retries,
                    c.stream_initial_retry_delay_ms,
                )
            })
            .unwrap_or((DEFAULT_AGENT_MAX_OUTPUT_TOKENS, 5, 1000));
        let opts = AgentRunOptions {
            model: model.to_string(),
            max_steps: if max_steps == 0 || max_steps == u32::MAX {
                DEFAULT_MAX_AGENT_STEPS
            } else {
                max_steps
            },
            max_output_tokens,
            stream_network_retries: stream_retries,
            stream_initial_retry_delay_ms: stream_delay,
            ..AgentRunOptions::default()
        };
        self.run_with_options(agent_id, task_description, &opts)
            .await
    }

    /// FM-15 Phase 3 主入口：携带 guardrail / timeout / max_steps 配置完整运行 Coding Agent。
    pub async fn run_with_options(
        &self,
        agent_id: &str,
        task_description: &str,
        opts: &AgentRunOptions,
    ) -> Result<AgentStatus> {
        let outer_dur = Duration::from_secs(opts.timeout_secs.max(1));
        match timeout(outer_dur, self.run_inner(agent_id, task_description, opts)).await {
            Ok(res) => res,
            Err(_) => {
                tracing::warn!(
                    "Agent {agent_id} hit wall-clock timeout ({:?}); marking failed",
                    outer_dur
                );
                let reason = format!("timeout: wall_clock {}s exceeded", opts.timeout_secs);
                self.emit_event(agent_id, 0, "error", &reason);
                self.emit_event(agent_id, 0, "status_change", "failed");
                self.update_agent_status(agent_id, "failed");
                self.expire_agent_notes(agent_id);
                self.mark_task_failed_with_reason(agent_id, "failed", &reason);
                Ok(AgentStatus::Failed)
            }
        }
    }

    async fn run_inner(
        &self,
        agent_id: &str,
        task_description: &str,
        opts: &AgentRunOptions,
    ) -> Result<AgentStatus> {
        // Debug log：每条关键事件都显式带上 agent_id / step，避免 span 跨 await
        // 的安全坑。日志格式 `agent={id} step={n} ...`，grep 即可拉单 agent 完整链路。
        tracing::info!(
            agent_id = %agent_id,
            model = %opts.model,
            max_steps = opts.max_steps,
            timeout_secs = opts.timeout_secs,
            max_output_tokens = opts.max_output_tokens,
            stream_network_retries = opts.stream_network_retries,
            task_desc_len = task_description.len(),
            "agent_run start"
        );

        let mut tools = coding_agent_tools_with_artifact_support();
        tools.extend(opts.extra_tools.clone());
        let workspace_dir = self.tool_executor.workspace_display();
        let contract_brief = render_task_contract_brief(opts.task_contract.as_ref());
        let guardrail_brief = render_guardrail_brief(&opts.guardrails);
        let produces_brief = render_produces_brief(&opts.produces);
        let expected_brief = opts
            .expected_output
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .map(|s| format!("\n\n## Expected Output\n{s}"))
            .unwrap_or_default();

        // FR-10: Codebase Intelligence —— 注入 [Project Structure] / [Tech Stack] /
        // [Upstream Context] / [Base Conflicts]。task_id 通过 agent_id 反查；任何步骤失败则
        // 该块为空，不阻塞 agent 启动。
        let db_state = self.app_handle.state::<Database>();
        let task_id_for_intel: Option<String> = db_state
            .with_conn(|conn| queries::get_task_id_for_agent(conn, agent_id))
            .ok()
            .flatten();
        let intel = codebase_intel::build_intel(
            &self.workspace_root,
            task_id_for_intel.as_deref(),
            Some(&db_state),
        );
        let intel_block = intel.render_system_block();
        let runtime_env_block =
            runtime_env::build_profile(&self.workspace_root).render_system_block();

        let delivery_contract = "\n\n## Delivery Contract\n\
             - If the task asks for a direct final answer, exact text, a JSON block, or says not to write files, put that requested answer itself in task_complete.summary. Do not summarize that you produced it; task_complete.summary is the final response seen by graders/users.\n\
             - If the task asks for a file, notebook, script, report, dataset, or generated output, persist the final answer in the requested artifact; do not rely on task_complete.summary as the only place where key answers exist.\n\
             - For statically evaluated notebooks, scripts, reports, and data files, make final constants, conclusions, required fields, and generated content visible in the source artifact or saved output. If you use derived expressions, keep the final values auditable in the artifact as visible comments, assertions, literals, or saved outputs while preserving the derivation.\n\
             - For generated artifacts, prefer small complete generators or incremental append chunks over one very large write_file payload. Escape nested quotes safely; if a generator fails near the step limit, overwrite it with a simpler complete generator instead of debugging it line-by-line.\n\
             - For presentations or PPTX files, make checklist requirements machine-readable in slide text: use plain ASCII/standard numbering such as `1.`, `2.`, `3.` or `①②③`, not decorative symbol numerals or icon-only bullets. Validate the saved presentation by extracting slide text from the file, not just by inspecting your source script.\n\
             - For long tasks, create required output skeletons early and update them after each major finding. Near the step or token limit, stop exploring and make the existing artifacts valid before completing.";

        let evidence_contract = "\n\n## Evidence Contract\n\
             - Large, medium, or repeated shell/read/search/list outputs may be returned as compact evidence refs instead of full text. The original output remains preserved in the referenced evidence path or agent events.\n\
             - Do not repeat broad fetch/cat/list commands only to see the same full output again. Use the evidence path, a narrower grep pattern, read_file with offset/limit, or a targeted extraction command.\n\
             - Treat [tool_result_ref] and [tool_result_repeat] as valid observations: use their source/hash/excerpt to decide the next focused retrieval step.";

        let system = format!(
            "You are a coding agent working in the directory: {workspace_dir}\n\n\
             ## Task\n{task_description}{expected_brief}{produces_brief}\n\n\
             ## Tools & Completion Protocol\n\
             - Use the provided tools to explore, read, write, and search files.\n\
             - All file paths are relative to the workspace root.\n\
             - When you have finished implementing the task and saved all files, you MUST call \
             the `task_complete` tool with a concise summary. \
             Do NOT just write a textual summary — only `task_complete` ends the task.\n\
             - Before calling `task_complete`, publish every artifact that was planned for this \
             task using `publish_artifact` (file_paths must point to files that already exist on disk).{contract_brief}{guardrail_brief}\n\
             - ALWAYS provide all required parameters when calling a tool.{runtime_env_block}{delivery_contract}{evidence_contract}{intel_block}"
        );

        let mut messages: Vec<Message> = Vec::new();
        let mut step: u32 = 0;
        let mut consecutive_no_tool: u32 = 0;
        let mut consecutive_read_only: u32 = 0;
        let mut hinted_read_only_loop = false;
        let mut retries_left: u32 = opts.guardrail_retry_budget;
        let mut hinted_remaining_steps = false;
        // Issue 3: idle-retry 预算（**step 级**）。
        //
        // 早期设计是"任务级"——整个 task 总共 2 次容错——结果一个 80-step 的任务
        // 偶发卡 3 次就直接 failed。Reseller 的真实卡频率（DeepSeek-V4 / SiliconFlow
        // Qwen）大约是每 10-20 step 一次，任务级 budget 等于把可恢复故障当不可恢复
        // 处理，违背 retry 的本意。
        //
        // 改成 step 级：每个 step 开始时把 budget 重置到 `opts.idle_retry_budget`。
        // 兜底 invariant：
        // - `max_steps`（默认 80）天然是 retry 总次数的隐式上限（每次 retry 都 step += 1）
        // - 每次 retry 都 emit `system_hint`，用户能在 Workspace 实时看到，可主动 cancel
        // - 与"每 step 是一次独立 LLM 调用"的语义对齐，更符合直觉
        let mut idle_retries_left: u32 = opts.idle_retry_budget;
        let mut resume_after_idle_retry = false;
        // Single-Agent Uplift P0-1: per-step 一次 reactive compact 配额。
        //
        // 不变量：每个 step 编号最多触发一次 reactive compact retry。如果 reactive
        // compact 后重发**仍然**撞 prompt_too_long → flag 守住不再压，让原错误 bail。
        //
        // 配合 `skip_step_increment` 实现：reactive retry 不消耗 step 号——同一 step
        // 内"失败 → 压 → 重发"是同一行 timeline 事件。下一个真正的 step++ 才把
        // attempted 重置回 false。
        //
        // 与 idle_retries_left 的差别：idle retry 每次 step++（即把 retry 当独立 step
        // 计入 max_steps 兜底），因为 idle 是"网络/上游卡住"，需要 max_steps 兜底
        // 防止无限 retry。reactive 不同——单 step 最多 1 次（flag 守住），不需要 step++
        // 兜底，反而想保持 step 号语义稳定（前端 timeline 一致性）。
        let mut attempted_reactive_compact_this_step = false;
        // P0-1 实现工具：设为 true 时本次 loop 跳过 step++ + flag reset 段，
        // 让 reactive compact 重发能复用同 step 号。仅 reactive 分支会 set，set 后
        // 立刻 continue；下一轮顶部消费完即清零。
        let mut skip_step_increment_for_reactive = false;
        // Single-Agent Uplift P0-2: 可选的 output_token budget tracker。
        //
        // `Some` = caller 显式开启（见 `AgentRunOptions::output_token_budget` 文档）；
        // `None` = 关闭，等同 P0-2 之前行为（仅 max_steps 兜底）。
        //
        // 不变量：tracker 只在每 step 拿到 LLM response 后 record + decide 一次。
        // **不**在 reactive compact retry 时重复 record（response 还没出来），
        // 也**不**在 IdleTimeout retry 时重复——这两个路径都 `continue` 跳过下方
        // `record_step` 调用。
        let mut budget_tracker: Option<BudgetTracker> =
            opts.output_token_budget.map(|_| BudgetTracker::new());
        // Single-Agent Uplift P1-3: max_output_tokens 三档恢复 state。
        //
        // - `current_max_output_tokens`：本 agent 当前用的 max_tokens；升档后**单调
        //   上升**直到 agent 结束。下一步直接用大窗口，减少撞顶概率。
        // - `escalated_once_this_step`：本 step 已经升过档（per-step flag，新 step 重置）；
        //   单 step 内只升一次，第二次就要走 multi-turn 而非死循环升档。
        // - `multi_turn_recovery_count`：multi-turn "Resume directly" 累计计数，**跨 step**
        //   累计；上限 `MAX_OUTPUT_TOKENS_RECOVERY_LIMIT` 后真 surface error。
        let mut current_max_output_tokens: u32 = opts.max_output_tokens;
        let mut escalated_once_this_step = false;
        let mut multi_turn_recovery_count: u32 = 0;
        let mut transient_network_retries_this_step: u32 = 0;
        // Single-Agent Uplift P1-2: Cross-Model Fallback state。
        //
        // - `current_model`：本 agent 当前 LLM 请求用的模型名。fallback 触发后切到
        //   opts.fallback_model；sticky=true 保留到 agent 结束，sticky=false 每 step
        //   末尾回切 primary。
        // - `switched_to_fallback_this_step`：本 step 已经切过一次（per-step flag）。
        //   防止主+备都过载时无限切换死循环 —— 本 step 切一次后不再切，整 step fail。
        // - `fallback_switches_total`：agent 维度累计切换次数，给 mission report /
        //   debug log 用。每次成功切换 +1。
        let mut current_model: String = opts.model.clone();
        let mut switched_to_fallback_this_step = false;
        let mut fallback_switches_total: u32 = 0;
        let required_output_files = required_file_outputs(opts);
        let run_started_at = std::time::Instant::now();
        let mut hinted_artifact_first = required_output_files.is_empty();
        let mut hinted_timeout_finalization = false;
        let mut guardrail_precheck_done = false;
        let mut guardrail_repair_active = false;
        let mut last_guardrail_repair_feedback: Option<String> = None;
        let mut max_steps_guardrail_repair_extended = false;
        let mut tool_result_context_state =
            crate::agent::tool_result_policy::ToolResultContextState::default();
        let mut invalid_tool_args_recovery_state = InvalidToolArgsRecoveryState::default();
        // Single-Agent Uplift P0-3: 等待 emit recovery_succeeded 的待办。
        //
        // 任意 recovery 分支（P0-1 reactive / idle retry / P1-3 escalate / multi-turn）
        // 触发后，往这里塞 (trigger, strategy)；下次 stream OK 完成时主循环消费并
        // emit recovery_succeeded。简化语义："下次 LLM 调用成功 = 上次 attempt 成功"。
        //
        // 不变量：
        //   - 永远只持有最近一次未 resolve 的 recovery；新 recovery 覆盖旧的（旧的
        //     视为"还在恢复中又遇到新故障"，最后一次 succeeded 也足够通知前端"现在 OK 了"）
        //   - 拿到 Stream OK 后立即取出 (take())，避免重复 emit
        let mut pending_recovery_to_resolve: Option<(RecoveryTrigger, RecoveryStrategy)> = None;

        self.emit_event(agent_id, step, "status_change", "running");
        self.update_agent_status(agent_id, "running");

        loop {
            // step 级 budget 重置：仅当本次迭代不是从 idle-retry continue 跳过来时
            // 才重置。语义已抽到 `next_idle_retry_budget` 单独写测试守住。
            idle_retries_left = next_idle_retry_budget(
                resume_after_idle_retry,
                idle_retries_left,
                opts.idle_retry_budget,
            );
            resume_after_idle_retry = false;
            // 与 idle_retries_left 不同：reactive compact 是"step 内"的自救——同一 step
            // 反复 continue 重发时**不能**重置（否则压完还失败时会无限循环）。step 边界
            // 才重置（即"开始新一步 LLM 调用"时）。
            //
            // 这里"边界"= step += 1 之前。注意 idle-retry continue 不 step++，所以这条
            // 判定要看 `resume_after_idle_retry` 的旧值（已经在前面读出并清零）。
            // 简化做法：在 step += 1 之后重置（见下方）。这里只 init。
            //
            // **不在循环顶部重置**：循环顶部对应"上一轮可能是 reactive compact retry"，
            // 重置会丢失"本 step 已经压过一次"的信息。step += 1 那行下面才是边界。

            if self.cancel_token.is_cancelled() {
                return self.finish_cancelled(agent_id, step);
            }

            if step >= opts.max_steps {
                if let Some(status) = self
                    .try_auto_complete_on_step_exhaustion(
                        agent_id,
                        step,
                        opts,
                        &required_output_files,
                        &mut messages,
                    )
                    .await?
                {
                    if status == AgentStatus::Running {
                        if !max_steps_guardrail_repair_extended && opts.guardrail_retry_budget > 0 {
                            self.emit_event_with_meta(
                                agent_id,
                                step,
                                "system_hint",
                                "Allowing one bounded repair turn after max-step auto-finalize guardrail feedback.",
                                Some(serde_json::json!({
                                    "kind": "max_steps_auto_finalize_repair_extension",
                                    "previous_max_steps": opts.max_steps,
                                })),
                            );
                            max_steps_guardrail_repair_extended = true;
                            guardrail_repair_active = true;
                            step = step.saturating_sub(1);
                        } else {
                            let reason = format!(
                                "max_steps: {} steps exhausted after auto-finalize guardrail feedback",
                                opts.max_steps
                            );
                            self.emit_event(agent_id, step, "error", &reason);
                            self.emit_event(agent_id, step, "status_change", "failed");
                            self.update_agent_status(agent_id, "failed");
                            self.expire_agent_notes(agent_id);
                            self.mark_task_failed_with_reason(agent_id, "failed", &reason);
                            return Ok(AgentStatus::Failed);
                        }
                    } else {
                        return Ok(status);
                    }
                }

                if !max_steps_guardrail_repair_extended
                    && opts.guardrail_retry_budget > 0
                    && messages.iter().rev().any(|message| {
                        message.content.iter().any(|block| {
                            matches!(
                                block,
                                ContentBlock::Text { text }
                                    if text.contains("[Guardrail Check Failed]")
                                        || text.contains("Late guardrail precheck failed")
                            )
                        })
                    })
                {
                    self.emit_event_with_meta(
                        agent_id,
                        step,
                        "system_hint",
                        "Allowing one bounded repair turn after max-step guardrail feedback.",
                        Some(serde_json::json!({
                            "kind": "max_steps_guardrail_repair_extension",
                            "previous_max_steps": opts.max_steps,
                        })),
                    );
                    max_steps_guardrail_repair_extended = true;
                    step = step.saturating_sub(1);
                } else {
                    let reason = format!(
                        "max_steps: {} steps exhausted without task_complete",
                        opts.max_steps
                    );
                    self.emit_event(agent_id, step, "error", &reason);
                    self.emit_event(agent_id, step, "status_change", "failed");
                    self.update_agent_status(agent_id, "failed");
                    self.expire_agent_notes(agent_id);
                    self.mark_task_failed_with_reason(agent_id, "failed", &reason);
                    return Ok(AgentStatus::Failed);
                }
            }

            let (_, missing_required_files) =
                required_files_status(&self.workspace_root, &required_output_files);
            if !hinted_artifact_first
                && step >= ARTIFACT_FIRST_HINT_STEP
                && !missing_required_files.is_empty()
            {
                hinted_artifact_first = true;
                let hint = artifact_first_hint(&missing_required_files, step);
                messages.push(Message {
                    role: MessageRole::User,
                    content: vec![ContentBlock::Text { text: hint.clone() }],
                    cache_control: None,
                });
                self.emit_event_with_meta(
                    agent_id,
                    step,
                    "system_hint",
                    &hint,
                    Some(serde_json::json!({
                        "kind": "artifact_first_hint",
                        "missing_required_files": missing_required_files,
                    })),
                );
            }

            let elapsed_secs = run_started_at.elapsed().as_secs();
            let timeout_secs = opts.timeout_secs.max(1);
            let remaining_wall_secs = timeout_secs.saturating_sub(elapsed_secs);
            let timeout_finalization_active =
                (elapsed_secs as f64) >= (timeout_secs as f64 * FINALIZATION_TIMEOUT_FRACTION);
            if timeout_finalization_active && !hinted_timeout_finalization {
                hinted_timeout_finalization = true;
                let hint = timeout_finalization_hint(remaining_wall_secs, &missing_required_files);
                messages.push(Message {
                    role: MessageRole::User,
                    content: vec![ContentBlock::Text { text: hint.clone() }],
                    cache_control: None,
                });
                self.emit_event_with_meta(
                    agent_id,
                    step,
                    "system_hint",
                    &hint,
                    Some(serde_json::json!({
                        "kind": "timeout_finalization_hint",
                        "remaining_wall_secs": remaining_wall_secs,
                        "missing_required_files": missing_required_files,
                    })),
                );
            }

            // 剩余步数 ≤ STEPS_REMAINING_HINT 时注入一条提示（一次性）
            if !hinted_remaining_steps
                && opts.max_steps > STEPS_REMAINING_HINT
                && opts.max_steps - step <= STEPS_REMAINING_HINT
            {
                hinted_remaining_steps = true;
                let hint = format!(
                    "[System] You have only {} steps left. Stop exploring now and finalize. \
                     If the task requires output files, create or update best-effort valid artifacts \
                     before calling task_complete; partial but well-formed artifacts are better than \
                     timing out with no output. Then call task_complete soon.",
                    opts.max_steps - step
                );
                messages.push(Message {
                    role: MessageRole::User,
                    content: vec![ContentBlock::Text { text: hint.clone() }],
                    cache_control: None,
                });
                self.emit_event(agent_id, step, "system_hint", &hint);
            }

            if !guardrail_precheck_done
                && opts.max_steps > GUARDRAIL_PRECHECK_REMAINING_STEPS
                && opts.max_steps - step <= GUARDRAIL_PRECHECK_REMAINING_STEPS
                && missing_required_files.is_empty()
            {
                guardrail_precheck_done = true;
                if self
                    .precheck_guardrails_for_repair(
                        agent_id,
                        step,
                        opts,
                        &required_output_files,
                        &mut messages,
                    )
                    .await
                {
                    guardrail_repair_active = true;
                    continue;
                }
            }

            // Single-Agent Uplift P0-1: reactive compact retry 复用同 step 号。
            // 同一 step 号可能对应最多 2 次 LLM 调用：原始 + 1 次 reactive 重发；
            // 前端 timeline 用 events 顺序而非 step 号区分这两次调用。
            if skip_step_increment_for_reactive {
                skip_step_increment_for_reactive = false;
                tracing::info!(
                    agent_id = %agent_id,
                    step,
                    msgs_in_context = messages.len(),
                    ctx_tokens_est = approximate_tokens(&messages),
                    "step retry (reactive compact)"
                );
            } else {
                step += 1;
                self.update_agent_step(agent_id, step);
                // 新 step 边界 → 重置 per-step flags。
                // - reactive compact 配额（P0-1）
                // - max_output_tokens 升档配额（P1-3）：每 step 又有一次升档机会
                //   （注意：current_max_output_tokens 不重置，是单调上升的——升过一次
                //    后下个 step 直接用大窗口）
                attempted_reactive_compact_this_step = false;
                transient_network_retries_this_step = 0;
                escalated_once_this_step = false;
                // P1-2: step 边界重置 fallback per-step flag。注意 current_model
                // 不在此重置 —— sticky=true 时跨 step 保留。
                switched_to_fallback_this_step = false;
                // P1-2 non-sticky: 上一步切了 fallback 但 sticky=false → 这一步回切 primary。
                // 大多数用户场景下 sticky=true 更合理（overload 通常持续几分钟），
                // 但 explicit opt-out 时尊重用户配置。
                if !opts.fallback_sticky && current_model != opts.model {
                    tracing::info!(
                        agent_id = %agent_id,
                        step,
                        from = %current_model,
                        to = %opts.model,
                        "P1-2 non-sticky: reverting to primary model at step boundary"
                    );
                    current_model = opts.model.clone();
                }
                tracing::info!(
                    agent_id = %agent_id,
                    step,
                    msgs_in_context = messages.len(),
                    ctx_tokens_est = approximate_tokens(&messages),
                    current_model = %current_model,
                    "step begin"
                );
            }

            // Single-Agent Uplift Phase 2.2 + B2: prompt 进 LLM 之前做三层瘦身。
            //   ① tool_summary：tool_summarizer 配置在则先尝试 LLM 摘要（小模型）
            //   ② tool_result 截尾：摘要失败/未启用的大块走传统 truncate
            //   ③ microcompact：整体 token 估算超 50K → 丢最早 1/3 messages 换 summary
            // 任一动作都 emit 对应事件让用户知情，避免 LLM 行为突变无解释。
            self.apply_tool_result_budget_with_optional_summary(agent_id, step, &mut messages)
                .await;
            let should_microcompact = approximate_tokens(&messages) > MICROCOMPACT_TOKEN_THRESHOLD;
            let should_working_memory =
                !should_microcompact && should_working_memory_compact(&messages);
            if should_microcompact || should_working_memory {
                // P2-1 Phase B: PreCompact hook 调用点。compact 即将丢弃部分历史，
                // hook 可以把关键 context 持久化到 artifact / agent_notes 防丢失。
                {
                    let hook_ctx = self.build_hook_context(
                        agent_id,
                        step,
                        crate::agent::hooks::HookPhase::PreCompact,
                        &messages,
                        None,
                        None,
                    );
                    // PreCompact 的 prevent 语义：terminal 仍然 fail；StepAborted 仅
                    // 跳过本次 compact（即不压缩，messages 保持原样进 LLM）——这对
                    // hook 想"今天先不压缩，让 model 看完再说"的场景是有意义的。
                    match self
                        .dispatch_hook_phase(agent_id, step, hook_ctx, &mut messages)
                        .await
                    {
                        Ok(()) => {
                            let compact_result = if should_working_memory {
                                working_memory_compact(&mut messages)
                            } else {
                                microcompact(&mut messages)
                            };
                            if let Some(report) = compact_result {
                                // P0-1 引入了 reactive compact 后，前端需要区分 proactive vs reactive
                                // 两种来源。proactive (这里) = 主动按本地 token 估算触发；
                                // reactive (`engine.rs` Llm 错误分支) = API 拒绝后兜底压缩。
                                let mut meta = report.to_meta();
                                if let Some(obj) = meta.as_object_mut() {
                                    obj.insert("kind".into(), serde_json::json!("proactive"));
                                    obj.insert(
                                        "trigger".into(),
                                        serde_json::json!(if should_working_memory {
                                            "working_memory"
                                        } else {
                                            "token_threshold"
                                        }),
                                    );
                                }
                                self.emit_event_with_meta(
                                    agent_id,
                                    step,
                                    "compact",
                                    &report.human_readable(),
                                    Some(meta),
                                );
                                // P2-1 Phase B: PostCompact hook 调用点。compact 完成
                                // （messages 已被 microcompact 改写），让 hook 重新注入丢失
                                // 的关键 context。Prevent 在此处都视为 fail——compact 已
                                // 不可逆，"不让 agent 继续"等价于 step abort 但 messages 已
                                // 改，无法回滚到 PreCompact 状态。
                                let hook_ctx_post = self.build_hook_context(
                                    agent_id,
                                    step,
                                    crate::agent::hooks::HookPhase::PostCompact,
                                    &messages,
                                    None,
                                    None,
                                );
                                match self
                                    .dispatch_hook_phase(
                                        agent_id,
                                        step,
                                        hook_ctx_post,
                                        &mut messages,
                                    )
                                    .await
                                {
                                    Ok(()) => {}
                                    Err(HookFatal::Terminal(reason))
                                    | Err(HookFatal::StepAborted(reason)) => {
                                        let msg =
                                            format!("PostCompact hook terminated agent: {reason}");
                                        self.mark_task_failed_with_reason(agent_id, "failed", &msg);
                                        self.emit_event(agent_id, step, "status_change", "failed");
                                        self.update_agent_status(agent_id, "failed");
                                        return Ok(AgentStatus::Failed);
                                    }
                                }
                            }
                        }
                        Err(HookFatal::Terminal(reason)) => {
                            let msg = format!("PreCompact hook terminated agent: {reason}");
                            self.mark_task_failed_with_reason(agent_id, "failed", &msg);
                            self.emit_event(agent_id, step, "status_change", "failed");
                            self.update_agent_status(agent_id, "failed");
                            return Ok(AgentStatus::Failed);
                        }
                        Err(HookFatal::StepAborted(reason)) => {
                            // 跳过 compact，继续走 LLM 调用（messages 未压缩，下次 token
                            // 估算如果仍超 threshold，下个 step 会再触发——但 hook 可以
                            // 用 matcher/cooldown 控制不无限触发）
                            tracing::info!(
                                agent_id = %agent_id,
                                step,
                                "PreCompact hook step-aborted compact: {reason}"
                            );
                        }
                    }
                }
            }

            // P2-1 Phase B: PreLlmCall hook 调用点。空 registry 时立即返回 Pass，
            // 无任何额外开销。Terminal prevent → 当作 step_aborted_fatal 走 fail 路径；
            // StepAborted → 跳过本 step 的 LLM 调用直接进入下一轮（loop continue）。
            {
                let hook_ctx = self.build_hook_context(
                    agent_id,
                    step,
                    crate::agent::hooks::HookPhase::PreLlmCall,
                    &messages,
                    None,
                    None,
                );
                match self
                    .dispatch_hook_phase(agent_id, step, hook_ctx, &mut messages)
                    .await
                {
                    Ok(()) => {}
                    Err(HookFatal::Terminal(reason)) => {
                        let msg = format!("PreLlmCall hook terminated agent: {reason}");
                        self.mark_task_failed_with_reason(agent_id, "failed", &msg);
                        self.emit_event(agent_id, step, "status_change", "failed");
                        self.update_agent_status(agent_id, "failed");
                        return Ok(AgentStatus::Failed);
                    }
                    Err(HookFatal::StepAborted(reason)) => {
                        tracing::warn!(
                            agent_id = %agent_id,
                            step,
                            "PreLlmCall hook aborted step (non-terminal): {reason}"
                        );
                        // 不调 LLM，但允许下个 step 继续（hook 注入的 message 仍在 messages
                        // 里——其实 StepAborted 不注入，但下一轮主循环会重新评估）
                        step = step.saturating_add(1);
                        continue;
                    }
                }
            }

            let call_summary = Self::describe_llm_call(step, &messages);
            let context_stats = collect_context_stats(&messages);
            let context_stats_meta =
                serde_json::to_value(&context_stats).unwrap_or_else(|_| serde_json::json!({}));
            self.emit_event_with_meta(
                agent_id,
                step,
                "llm_call",
                &call_summary,
                Some(serde_json::json!({ "context_stats": context_stats_meta.clone() })),
            );
            self.emit_event_with_meta(
                agent_id,
                step,
                "context_stats",
                &format!(
                    "messages={} blocks={} approx_tokens={} tool_result_chars={} largest_tool_result_chars={}",
                    context_stats.message_count,
                    context_stats.block_count,
                    context_stats.approx_tokens,
                    context_stats.tool_result_chars,
                    context_stats.largest_tool_result_chars,
                ),
                Some(context_stats_meta),
            );

            let request = LlmRequest {
                // P1-2: 用 current_model 而非 opts.model。fallback 切换后这里直接拿
                // 备用模型；sticky=true 时跨 step 保持，sticky=false 由 step 末
                // 段恢复 primary。
                model: current_model.clone(),
                system: Some(system.clone()),
                messages: messages.clone(),
                tools: tools.clone(),
                // P1-3: 用 current_max_output_tokens 而非 opts.max_output_tokens。
                // 升档后这个值会上升到 ESCALATED_MAX_OUTPUT_TOKENS（clamped）；
                // 没撞顶过则保持初始值。
                max_tokens: current_max_output_tokens,
                provider_extras: None,
            };
            tracing::info!(
                agent_id = %agent_id,
                step,
                model = %current_model,
                msg_count = request.messages.len(),
                tool_count = request.tools.len(),
                max_tokens = request.max_tokens,
                "llm_request dispatch"
            );

            let (tx, mut rx) = mpsc::channel::<StreamChunk>(256);
            let agent_id_owned = agent_id.to_string();
            let app_handle = self.app_handle.clone();
            let stream_step = step;
            // Single-Agent Uplift Phase 0.4: shared 活动计时器。
            // 用 AtomicU64 存自 step 开始以来的毫秒数；每收到一个 chunk 更新它。
            // heartbeat 任务读它来判断是否进入"看起来卡住"状态。
            let step_started_at = std::time::Instant::now();
            let last_chunk_at = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
            let last_chunk_at_fwd = last_chunk_at.clone();
            // Debug log：跟踪 chunk 抵达节奏。能区分"首 token 慢"vs"流中途变慢"vs
            // "整体很快但量大"——三种用户感知都是"卡住"，但根因不同。
            //
            // chunks_seen 给 heartbeat 任务用：决定文案是"等首 token"还是"流式接收中"。
            let trace_agent_id = agent_id.to_string();
            let trace_step = step;
            let chunks_seen = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
            let chunks_seen_fwd = chunks_seen.clone();
            let forwarder = tokio::spawn(async move {
                let mut text_chunks: u64 = 0;
                let mut text_bytes: u64 = 0;
                let mut reasoning_chunks: u64 = 0;
                let mut reasoning_bytes: u64 = 0;
                let mut first_chunk_logged = false;
                while let Some(chunk) = rx.recv().await {
                    let elapsed_ms = step_started_at.elapsed().as_millis() as u64;
                    last_chunk_at_fwd.store(elapsed_ms, std::sync::atomic::Ordering::Relaxed);
                    chunks_seen_fwd.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if !first_chunk_logged {
                        first_chunk_logged = true;
                        tracing::info!(
                            agent_id = %trace_agent_id,
                            step = trace_step,
                            first_chunk_ms = elapsed_ms,
                            chunk_kind = ?chunk.kind,
                            chunk_bytes = chunk.content.len(),
                            "llm_stream first chunk arrived"
                        );
                    }
                    // TextDelta：主回显流；ReasoningDelta：thinking 阶段，前端渲染成
                    // "思考中..."占位卡，避免推理模型 thinking 期间用户看到长沉默。
                    //
                    // P1-1 Phase B (observability-first)：ToolUseStart / ToolUseStop
                    // 也在 stream 中转发给前端，让 UI 在 LLM 还没收完时就提前显示
                    // "🔧 read_file 已开始"。**执行时机不变**——actual dispatch 仍在
                    // step 末尾 batch 跑，避免触碰并发竞态（cancel / approval gate
                    // 还无法处理"safe tool 跑一半 stream cancel"边界）。
                    //
                    // 这给用户带来"看起来快了 1-3s"的感知 win，且 0 风险：execution
                    // 路径 100% 不变。真正的 streaming overlap 留到未来 PR（需要
                    // StreamingToolExecutor 接入 ToolDispatcher trait + 单测覆盖
                    // cancel/approval 边界）。
                    let payload_meta: Option<serde_json::Value>;
                    let kind_str = match &chunk.kind {
                        StreamChunkKind::TextDelta => {
                            text_chunks += 1;
                            text_bytes += chunk.content.len() as u64;
                            payload_meta = None;
                            "text_delta"
                        }
                        StreamChunkKind::ReasoningDelta => {
                            reasoning_chunks += 1;
                            reasoning_bytes += chunk.content.len() as u64;
                            payload_meta = None;
                            "reasoning_delta"
                        }
                        StreamChunkKind::ToolUseStart { tool_use_id, name } => {
                            // tool_use_started 给前端早渲染 spinner。content 字段传
                            // 工具名让 ToolUseLine 立刻能显示，不必等 batch emit 后才出现。
                            payload_meta = Some(serde_json::json!({
                                "tool_use_id": tool_use_id,
                                "tool_name": name,
                                "phase": "started",
                            }));
                            "tool_use_started"
                        }
                        StreamChunkKind::ToolUseStop { tool_use_id } => {
                            // tool_use_arg_done 标志 LLM 已完整输出该 tool_use 的 input。
                            // 前端可以把 "args 解析中" → "args 完成、等执行" 状态切换。
                            payload_meta = Some(serde_json::json!({
                                "tool_use_id": tool_use_id,
                                "phase": "arg_done",
                            }));
                            "tool_use_arg_done"
                        }
                        // InputDelta 不发——前端不需要看 JSON 半成品；MessageStop 同理
                        StreamChunkKind::ToolUseInputDelta { .. }
                        | StreamChunkKind::MessageStop => continue,
                    };
                    let _ = app_handle.emit(
                        "agent-stream",
                        AgentEventPayload {
                            agent_id: agent_id_owned.clone(),
                            step: stream_step,
                            kind: kind_str.to_string(),
                            content: chunk.content,
                            meta: payload_meta,
                        },
                    );
                }
                let total_ms = step_started_at.elapsed().as_millis() as u64;
                tracing::info!(
                    agent_id = %trace_agent_id,
                    step = trace_step,
                    text_chunks,
                    text_bytes,
                    reasoning_chunks,
                    reasoning_bytes,
                    total_ms,
                    avg_chunk_size = if text_chunks > 0 { text_bytes / text_chunks } else { 0 },
                    "llm_stream forwarder done"
                );
            });

            // Single-Agent Uplift Phase 0.4: Heartbeat。
            // LLM stream 静默 ≥ HEARTBEAT_IDLE_SECS 时往前端推一条 tool_progress，
            // 让用户实时看到"⏳ 等待 LLM 已 30s..."而不是干瞪着空 terminal。
            //
            // 抢先于 stream_chat_with_idle_guard 的 180s abort —— 这条事件不杀流，
            // 只是知会用户。状态的真正终止仍由 idle_guard 决定。
            let heartbeat_cancel = std::sync::Arc::new(tokio::sync::Notify::new());
            let heartbeat_cancel_for_task = heartbeat_cancel.clone();
            let heartbeat_app = self.app_handle.clone();
            let heartbeat_agent_id = agent_id.to_string();
            let heartbeat_step = step;
            let heartbeat_last_chunk_at = last_chunk_at.clone();
            let heartbeat_chunks_seen = chunks_seen.clone();
            let heartbeat_handle = tokio::spawn(async move {
                /// 静默多少秒触发第一次 heartbeat。从 llm::stream_guard 单源取值，
                /// 避免两边阈值漂移。30s 是用户开始疑神疑鬼的临界点。
                const HEARTBEAT_IDLE_SECS: u64 = DEFAULT_STREAM_IDLE_HEARTBEAT_SECS;
                /// 之后每 N 秒再推一次。频率太高刷屏，太低不及时。
                const HEARTBEAT_REPEAT_SECS: u64 = 15;
                let mut last_emitted_at_ms: Option<u64> = None;
                loop {
                    tokio::select! {
                        _ = heartbeat_cancel_for_task.notified() => break,
                        _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => {
                            let now_ms = step_started_at.elapsed().as_millis() as u64;
                            let last_ms = heartbeat_last_chunk_at.load(std::sync::atomic::Ordering::Relaxed);
                            let idle_ms = now_ms.saturating_sub(last_ms);
                            let idle_secs = idle_ms / 1000;
                            if idle_secs < HEARTBEAT_IDLE_SECS {
                                continue;
                            }
                            // 距离上次 emit 不到 HEARTBEAT_REPEAT_SECS 时不重复
                            if let Some(prev) = last_emitted_at_ms {
                                if now_ms.saturating_sub(prev) < HEARTBEAT_REPEAT_SECS * 1000 {
                                    continue;
                                }
                            }
                            last_emitted_at_ms = Some(now_ms);
                            // 文案细分：还没收到任何 chunk → "等首 token"（推理模型 thinking 中）
                            //          已经收到过 chunk → "流式接收中但暂停"（reseller 慢吞吞）
                            // 两种场景用户都看到沉默，但根因和应对策略完全不同：
                            //   - 等首 token：可能是 thinking 模型，正常；或网络握手卡，需要重启
                            //   - 流中途暂停：通常是 reseller buffer 慢 flush，等等就会继续
                            let chunks = heartbeat_chunks_seen.load(std::sync::atomic::Ordering::Relaxed);
                            let (content, phase) = if chunks == 0 {
                                (
                                    format!(
                                        "Waiting for LLM first token... {idle_secs}s elapsed \
                                         (reasoning models can take 60+s; network/provider may be slow)"
                                    ),
                                    "awaiting_first_token",
                                )
                            } else {
                                (
                                    format!(
                                        "LLM streaming paused — last chunk {idle_secs}s ago \
                                         ({chunks} chunks received so far)"
                                    ),
                                    "stream_stalled",
                                )
                            };
                            let meta = serde_json::json!({
                                "kind": "llm_idle",
                                "phase": phase,
                                "idle_secs": idle_secs,
                                "chunks_seen": chunks,
                                "step": heartbeat_step,
                            });
                            // heartbeat 只 emit 推送，不持久化 —— 持久化后每个 step 都
                            // 会留一堆"还在等"事件，污染 timeline。前端"实时"足矣。
                            let _ = heartbeat_app.emit(
                                "agent-event",
                                AgentEventPayload {
                                    agent_id: heartbeat_agent_id.clone(),
                                    step: heartbeat_step,
                                    kind: "tool_progress".to_string(),
                                    content,
                                    meta: Some(meta),
                                },
                            );
                        }
                    }
                }
            });

            // Idle 看门狗统一走 llm::stream_guard：长沉默 abort，避免 agent
            // 单步永远卡死整个任务（之前完全没有空闲保护）。
            //
            // Issue 3: IdleTimeout 不再立即 fail —— 还有 idle_retries_left 时注入
            // 一条 "[System] 上一次响应中断，请继续" 的 user 提示，下一次 loop
            // 重新发起 LLM 调用。这模拟用户在 Cursor / Claude Desktop 按 "continue"
            // 的体验，对偶发卡住的 reseller（DeepSeek-V4 / SiliconFlow Qwen）尤其有效。
            // 其他错误（Llm / Join）保持原失败路径。
            //
            // Cancel 联动：把 self.cancel_token 传进 guard，用户点"停止"时 stream
            // **立即** abort 而不是等当前 step 跑完——之前 cancel 只在 step 边界检查，
            // 体感最坏要等 180s（一次 idle_timeout）才停。
            let retry_policy = StreamRetryPolicy {
                max_retries: opts.stream_network_retries,
                initial_backoff: std::time::Duration::from_millis(
                    opts.stream_initial_retry_delay_ms,
                ),
                max_backoff: std::time::Duration::from_secs(16),
            };
            let stream_outcome = stream_chat_with_idle_guard_full(
                self.provider.clone(),
                request,
                tx,
                DEFAULT_STREAM_IDLE_TIMEOUT,
                self.cancel_token.clone(),
                retry_policy,
            )
            .await;
            let stream_total_ms = step_started_at.elapsed().as_millis() as u64;
            let _ = forwarder.await;
            heartbeat_cancel.notify_waiters();
            let _ = heartbeat_handle.await;
            let response = match stream_outcome {
                Ok(r) => {
                    let tool_use_n = r
                        .content
                        .iter()
                        .filter(|b| matches!(b, ContentBlock::ToolUse { .. }))
                        .count();
                    tracing::info!(
                        agent_id = %agent_id,
                        step,
                        stop_reason = %r.stop_reason,
                        input_tokens = r.usage.input_tokens,
                        output_tokens = r.usage.output_tokens,
                        stream_total_ms,
                        tool_use_blocks = tool_use_n,
                        "llm_response done"
                    );
                    // P0-3: 如果上一次循环触发了 recovery_attempt，这次 stream 成功
                    // 完成 → emit 一条 silent recovery_succeeded 给前端关闭"恢复中"状态。
                    if let Some((trigger, strategy)) = pending_recovery_to_resolve.take() {
                        let content = format_succeeded_content(trigger, &strategy);
                        let meta = build_recovery_succeeded_meta(trigger, &strategy);
                        self.emit_event_with_meta(
                            agent_id,
                            step,
                            "recovery_succeeded",
                            &content,
                            Some(meta),
                        );
                    }
                    r
                }
                Err(StreamGuardError::IdleTimeout {
                    idle_secs,
                    threshold_secs,
                }) if idle_retries_left > 0 => {
                    idle_retries_left -= 1;
                    // 关键：标记下一次 loop 是"延续本 step 的 retry"，否则 loop 顶部
                    // 会把 budget 重置回满，等于无限 retry。
                    resume_after_idle_retry = true;
                    let notice = format!(
                        "LLM stream idle for {idle_secs}s (threshold {threshold_secs}s); auto-continue ({} retries left this step)",
                        idle_retries_left
                    );
                    tracing::warn!(
                        agent_id = %agent_id,
                        step,
                        idle_secs,
                        threshold_secs,
                        retries_left = idle_retries_left,
                        "stream idle timeout, auto-injecting continue prompt"
                    );
                    self.emit_event(agent_id, step, "system_hint", &notice);
                    // P0-3: 同时 emit silent recovery_attempt 给前端开"恢复中"状态。
                    // **双发**：保留 system_hint 给现有 UI 渲染（用户可见），同时新发
                    // recovery_attempt 给未来 toggle 后的 UX 用。前端切换后 system_hint 可删。
                    let strategy = RecoveryStrategy::IdleRetryContinue {
                        retries_left: idle_retries_left,
                    };
                    let attempt_meta = build_recovery_attempt_meta(
                        RecoveryTrigger::IdleTimeout,
                        &strategy,
                        &format!("idle {idle_secs}s > {threshold_secs}s threshold"),
                        1,
                    );
                    self.emit_event_with_meta(
                        agent_id,
                        step,
                        "recovery_attempt",
                        &format_attempt_content(RecoveryTrigger::IdleTimeout, &strategy),
                        Some(attempt_meta),
                    );
                    pending_recovery_to_resolve = Some((
                        RecoveryTrigger::IdleTimeout,
                        RecoveryStrategy::IdleRetryContinue {
                            retries_left: idle_retries_left,
                        },
                    ));
                    // 没有 assistant turn 可 push（流被中止）。直接追加一条 user
                    // 提示给 LLM 让它在下一次 stream 里基于已有上下文继续。
                    let continue_msg = format!(
                        "[System] 上一次响应在 {idle_secs}s 后中断未输出完整内容。请基于到目前为止的对话上下文继续完成任务；\
                         不需要重复你已经说过的内容，直接接着写。如果上次正打算调用工具，请重新调用一次。"
                    );
                    messages.push(Message {
                        role: MessageRole::User,
                        content: vec![ContentBlock::Text { text: continue_msg }],
                        cache_control: None,
                    });
                    continue;
                }
                Err(StreamGuardError::Cancelled) => {
                    return self.finish_cancelled(agent_id, step);
                }
                // Single-Agent Uplift P0-1: API 端真返回 "prompt too long" 类错误 →
                // 同 step 内做一次 reactive compact 重发。压不动（messages 太少）/已经
                // 压过一次（flag set） → 走原 bail 路径让上层 retry 或 fail。
                //
                // 判定逻辑放在 `crate::llm::error_class::classify_llm_error`，按错误消息
                // 关键词识别 OpenAI / Anthropic / DeepSeek / 通义 四家的 context-length
                // 错误。**不识别** rate_limit / overload —— 那些走 P1-2 model fallback
                // （现阶段仍走默认 bail）。
                Err(StreamGuardError::Llm(msg))
                    if !attempted_reactive_compact_this_step
                        && crate::llm::classify_llm_error(&msg)
                            == crate::llm::LlmErrorClass::PromptTooLong =>
                {
                    attempted_reactive_compact_this_step = true;
                    let report = reactive_compact_aggressive(&mut messages);
                    if let Some(r) = &report {
                        // 成功路径：emit silent recovery_attempt（前端默认隐藏）
                        // 同时保留 'compact' event 以兼容旧前端渲染——双发不影响数据流，
                        // P0-3 前端 toggle 上线后可以删 compact 那条。
                        let strategy = RecoveryStrategy::ReactiveCompact {
                            dropped_msgs: r.dropped_messages,
                            tokens_before: r.tokens_before,
                            tokens_after: r.tokens_after,
                        };
                        let attempt_meta = build_recovery_attempt_meta(
                            RecoveryTrigger::PromptTooLong,
                            &strategy,
                            &msg,
                            1,
                        );
                        let content =
                            format_attempt_content(RecoveryTrigger::PromptTooLong, &strategy);
                        self.emit_event_with_meta(
                            agent_id,
                            step,
                            "recovery_attempt",
                            &content,
                            Some(attempt_meta),
                        );
                        // 同时保留 compact 事件（兼容现有前端 timeline 渲染）
                        let mut compact_meta = r.to_meta();
                        if let Some(obj) = compact_meta.as_object_mut() {
                            obj.insert("kind".into(), serde_json::json!("reactive"));
                            obj.insert("trigger".into(), serde_json::json!("prompt_too_long"));
                            obj.insert(
                                "error_excerpt".into(),
                                serde_json::json!(msg.chars().take(200).collect::<String>()),
                            );
                        }
                        self.emit_event_with_meta(
                            agent_id, step, "compact",
                            &format!(
                                "Reactive compact: dropped {} msg (~{}K → ~{}K tokens) after API context-length error",
                                r.dropped_messages, r.tokens_before / 1000, r.tokens_after / 1000
                            ),
                            Some(compact_meta),
                        );
                    } else {
                        // 失败路径：messages 已经太短压不动 → emit error（用户可见），原 bail
                        let human = format!(
                            "Cannot recover from prompt_too_long: only {} message(s) remain, no further compaction possible. Failing step.",
                            messages.len()
                        );
                        self.emit_event_with_meta(
                            agent_id,
                            step,
                            "error",
                            &human,
                            Some(serde_json::json!({
                                "kind": "reactive_compact_no_room",
                                "messages_remaining": messages.len(),
                                "error_excerpt": msg.chars().take(200).collect::<String>(),
                            })),
                        );
                        return Err(anyhow::anyhow!(msg));
                    }
                    tracing::warn!(
                        agent_id = %agent_id,
                        step,
                        msgs_after_compact = messages.len(),
                        ctx_tokens_after = approximate_tokens(&messages),
                        "reactive compact succeeded; retrying same step"
                    );
                    // 标记 pending recovery：下一次 stream 成功完成时 emit succeeded
                    pending_recovery_to_resolve = Some((
                        RecoveryTrigger::PromptTooLong,
                        RecoveryStrategy::ReactiveCompact {
                            dropped_msgs: report.as_ref().map(|r| r.dropped_messages).unwrap_or(0),
                            tokens_before: report.as_ref().map(|r| r.tokens_before).unwrap_or(0),
                            tokens_after: report.as_ref().map(|r| r.tokens_after).unwrap_or(0),
                        },
                    ));
                    // **关键**：不 step++，复用本 step 编号重发请求。step 号在前端 timeline
                    // 保持单调，"同一步 LLM 调用失败 → 压缩 → 重发"看起来是一条线的事件流。
                    skip_step_increment_for_reactive = true;
                    continue;
                }
                // Single-Agent Uplift P1-2: Overloaded / RateLimited + 配置了 fallback_model
                // + 本 step 还没切过 + fallback 与当前模型不同 → 切到 fallback 重发同 step。
                //
                // 优先级：PromptTooLong 优先匹配（上面的 arm），fallback 这条只在
                // PromptTooLong 不匹配时进入。这与 LlmErrorClass 内部的优先级一致。
                //
                // 单 step 一次的硬约束：避免 primary+fallback 都过载时无限切换。
                // 第二次再遇到 trigger → fall through 到 Err(e) 的 bail 路径，整 step 失败。
                Err(StreamGuardError::Llm(msg))
                    if !switched_to_fallback_this_step
                        && opts.fallback_model.is_some()
                        && opts.fallback_model.as_deref() != Some(current_model.as_str())
                        && crate::llm::classify_llm_error(&msg).is_fallback_trigger() =>
                {
                    let class = crate::llm::classify_llm_error(&msg);
                    let trigger = RecoveryTrigger::from_error_class(class)
                        .expect("is_fallback_trigger guarantees Some(trigger)");
                    let from_model = current_model.clone();
                    let to_model = opts
                        .fallback_model
                        .clone()
                        .expect("guarded by opts.fallback_model.is_some()");
                    switched_to_fallback_this_step = true;
                    fallback_switches_total += 1;
                    current_model = to_model.clone();

                    // strip reasoning blocks: 跨模型 thinking signature 不通用，
                    // 不剥会让 fallback 拿到上一个模型的 thinking 后 400/silent-drop
                    crate::agent::fallback::strip_reasoning_blocks(&mut messages);

                    let strategy = RecoveryStrategy::ModelFallback {
                        from: from_model.clone(),
                        to: to_model.clone(),
                        switch_total: fallback_switches_total,
                    };
                    // 双发：silent recovery_attempt + 可见 system_hint。
                    // 与 reactive compact 不同——fallback 改变了请求的模型，**用户应该
                    // 知道**（成本/能力可能差异大），所以 system_hint 这条不静默。
                    let attempt_meta = build_recovery_attempt_meta(trigger, &strategy, &msg, 1);
                    self.emit_event_with_meta(
                        agent_id,
                        step,
                        "recovery_attempt",
                        &format_attempt_content(trigger, &strategy),
                        Some(attempt_meta),
                    );
                    let visible_msg = format!(
                        "Primary model `{from_model}` returned {trigger_label}; switched to fallback `{to_model}` and retrying this step.",
                        trigger_label = class.as_str(),
                    );
                    let switch_meta = crate::agent::fallback::FallbackSwitchMeta {
                        from: from_model.clone(),
                        to: to_model.clone(),
                        trigger: class.as_str(),
                        switch_in_step: 1,
                        switch_total: fallback_switches_total,
                    };
                    self.emit_event_with_meta(
                        agent_id,
                        step,
                        "system_hint",
                        &visible_msg,
                        Some(switch_meta.to_json()),
                    );
                    pending_recovery_to_resolve = Some((trigger, strategy));
                    // P1-2 Phase B: 立即持久化累计计数。放在这里而非 agent 结束时，
                    // 因为 agent 可能在 fallback 后崩溃；持久化点放在"切换那一刻"
                    // 保证 report / 长期统计永远不漏。开销可忽略——单 row 单字段 UPDATE。
                    self.persist_fallback_switches(agent_id, fallback_switches_total);
                    tracing::warn!(
                        agent_id = %agent_id,
                        step,
                        from = %from_model,
                        to = %to_model,
                        trigger = %class.as_str(),
                        fallback_switches_total,
                        "cross-model fallback triggered, retrying same step"
                    );
                    // 同 step 重发：用 fallback model，复用 step 号（与 reactive 一致策略）
                    skip_step_increment_for_reactive = true;
                    continue;
                }
                Err(StreamGuardError::Llm(msg))
                    if llm_error_looks_transient_network(&msg)
                        && transient_network_retries_this_step < opts.stream_network_retries =>
                {
                    transient_network_retries_this_step += 1;
                    let delay = transient_network_retry_delay(
                        transient_network_retries_this_step,
                        opts.stream_initial_retry_delay_ms,
                    );
                    let notice = format!(
                        "LLM request failed with a transient network error; retrying step {step} (attempt {}/{}) after {}ms.",
                        transient_network_retries_this_step,
                        opts.stream_network_retries,
                        delay.as_millis()
                    );
                    tracing::warn!(
                        agent_id = %agent_id,
                        step,
                        attempt = transient_network_retries_this_step,
                        max_attempts = opts.stream_network_retries,
                        retry_delay_ms = delay.as_millis() as u64,
                        error = %msg,
                        "transient LLM network error; retrying same step"
                    );
                    self.emit_event_with_meta(
                        agent_id,
                        step,
                        "recovery_attempt",
                        &notice,
                        Some(serde_json::json!({
                            "trigger": "transient_network_error",
                            "attempt": transient_network_retries_this_step,
                            "max_attempts": opts.stream_network_retries,
                            "retry_delay_ms": delay.as_millis() as u64,
                            "error_excerpt": msg.chars().take(240).collect::<String>(),
                        })),
                    );
                    tokio::time::sleep(delay).await;
                    skip_step_increment_for_reactive = true;
                    continue;
                }
                Err(e) => return Err(anyhow::anyhow!(e.user_message_zh())),
            };

            if self.cancel_token.is_cancelled() {
                return self.finish_cancelled(agent_id, step);
            }

            messages.push(Message {
                role: MessageRole::Assistant,
                content: response.content.clone(),
                cache_control: None,
            });
            if let Some(text) = assistant_text_from_blocks(&response.content) {
                self.emit_event(agent_id, step, "assistant_text", &text);
            }

            // P2-1 Phase B: PostSampling hook 调用点。assistant message 已 push 进
            // messages，tool_use 解析尚未开始。典型用途：记录响应文本 / reasoning 长度分析。
            // 在 cost 记录前调，让 hook 也能阻止"按 token 计费但不该继续"的边缘场景。
            {
                let hook_ctx = self.build_hook_context(
                    agent_id,
                    step,
                    crate::agent::hooks::HookPhase::PostSampling,
                    &messages,
                    None,
                    None,
                );
                match self
                    .dispatch_hook_phase(agent_id, step, hook_ctx, &mut messages)
                    .await
                {
                    Ok(()) => {}
                    Err(HookFatal::Terminal(reason)) => {
                        let msg = format!("PostSampling hook terminated agent: {reason}");
                        self.mark_task_failed_with_reason(agent_id, "failed", &msg);
                        self.emit_event(agent_id, step, "status_change", "failed");
                        self.update_agent_status(agent_id, "failed");
                        return Ok(AgentStatus::Failed);
                    }
                    Err(HookFatal::StepAborted(_)) => {
                        step = step.saturating_add(1);
                        continue;
                    }
                }
            }

            let step_cost = self.provider.estimate_cost(
                &opts.model,
                response.usage.input_tokens,
                response.usage.output_tokens,
            );
            self.persist_cost_record(
                agent_id,
                &opts.model,
                response.usage.input_tokens,
                response.usage.output_tokens,
                step_cost,
            );
            self.accumulate_agent_cost(
                agent_id,
                response.usage.input_tokens,
                response.usage.output_tokens,
                step_cost,
            );

            // Single-Agent Uplift P0-2: 累计 output token 到 budget tracker。
            // 仅当 caller 显式开启 budget（opts.output_token_budget=Some）时 tracker
            // 才存在。decide 调用放在 task_complete 判定之前（见下方）——这样如果
            // budget 触发，nudge 能赶在本 step 的 follow-up message 里一起回给 LLM。
            if let Some(t) = budget_tracker.as_mut() {
                t.record_step(response.usage.output_tokens);
            }

            // FM-14: budget gate —— 累计成本触线时阻塞当前 agent 等待审批。
            // rejected → 标 task failed 让 mission 自然走完终态判定。
            // approved / 触发不到（ratio=0 或未签 contract）→ 静默继续。
            if let Some(coord) = self
                .app_handle
                .try_state::<std::sync::Arc<crate::agent::approval::ApprovalCoordinator>>()
            {
                let db = self.app_handle.state::<Database>();
                let mission_id_opt: Option<String> = db
                    .with_conn(|conn| queries::get_mission_id_for_agent(conn, agent_id))
                    .ok()
                    .flatten();
                if let Some(mission_id) = mission_id_opt {
                    use crate::agent::approval::ApprovalDecision;
                    use crate::agent::approval_gate::maybe_trigger_budget;
                    if let Some(decision) = maybe_trigger_budget(
                        &self.app_handle,
                        coord.inner(),
                        db.inner(),
                        &self.cancel_token,
                        &mission_id,
                        agent_id,
                    )
                    .await
                    {
                        if matches!(
                            decision,
                            ApprovalDecision::Rejected | ApprovalDecision::Cancelled
                        ) {
                            let reason = "budget: user rejected continuation past warn threshold";
                            self.emit_event(agent_id, step, "error", reason);
                            self.emit_event(agent_id, step, "status_change", "failed");
                            self.update_agent_status(agent_id, "failed");
                            self.expire_agent_notes(agent_id);
                            self.mark_task_failed_with_reason(agent_id, "failed", reason);
                            return Ok(AgentStatus::Failed);
                        }
                    }
                }
            }

            self.emit_event(
                agent_id,
                step,
                "checkpoint",
                &format!(
                    "tokens: {}in/{}out | cost: ${:.4} | stop: {}",
                    response.usage.input_tokens,
                    response.usage.output_tokens,
                    step_cost,
                    response.stop_reason
                ),
            );

            // 先解析 tool_use_blocks，因为下面 max_tokens hint 的注入路径要根据
            // "本步有没有 tool_calls" 选择"独立 user message" 还是 "并入 follow-up"。
            let mut tool_use_blocks: Vec<(String, String, serde_json::Value)> = Vec::new();
            for block in &response.content {
                if let ContentBlock::ToolUse { id, name, input } = block {
                    tool_use_blocks.push((id.clone(), name.clone(), input.clone()));
                }
            }

            // Single-Agent Uplift P1-3: 三档 max_output_tokens 恢复。
            //
            // 当 LLM 因 max_tokens 被截断 (`stop_reason ∈ {"length","max_tokens","max_output_tokens"}`)，
            // 按优先级走以下分支：
            //   ① **升档**：本 step 未升过 + 当前 cap < ESCALATED → 升到 ESCALATED 重发同 prompt
            //      （需 strip 上次 partial assistant message + skip_step_increment_for_reactive 复用同 step 号）
            //   ② **multi-turn recovery**：升档过 / 已在 ESCALATED → 注入 "Resume directly" 提示让 LLM 接着写
            //      （保留 assistant message + 走 follow-up / 独立 push 路径）
            //   ③ **surface**：multi-turn 已用满 LIMIT 次 → emit error 事件（不 fail，但不再恢复）
            //
            // 注入协议（同 reactive）：
            //   - 本步有 tool_calls → 暂存到 pending_max_tokens_hint → follow-up 阶段并入
            //   - 本步无 tool_calls → 直接 push 独立 user msg
            //
            // ① 走 continue（不到下面 push assistant 处理）——它独立于 ②/③ 的注入路径。
            let mut pending_max_tokens_hint: Option<String> = None;
            let is_max_tokens_hit = matches!(
                response.stop_reason.as_str(),
                "length" | "max_tokens" | "max_output_tokens"
            );

            if is_max_tokens_hit {
                // ① 升档分支：可以升 + 没升过
                let escalated_cap = compute_escalated_cap(
                    self.provider.name(),
                    &opts.model,
                    current_max_output_tokens,
                );
                let can_escalate =
                    !escalated_once_this_step && escalated_cap > current_max_output_tokens;
                if can_escalate {
                    let old_cap = current_max_output_tokens;
                    let new_cap = escalated_cap;
                    escalated_once_this_step = true;
                    current_max_output_tokens = new_cap;

                    self.emit_event_with_meta(
                        agent_id,
                        step,
                        "system_hint",
                        &format!(
                            "Output truncated at {old_cap} tokens — auto-escalating max_output_tokens to {new_cap} and retrying same step"
                        ),
                        Some(serde_json::json!({
                            "kind": "max_tokens_escalate",
                            "old_cap": old_cap,
                            "new_cap": new_cap,
                            "stop_reason": response.stop_reason,
                        })),
                    );
                    // P0-3: 双发 silent recovery_attempt（同 reactive / idle 模式）
                    let strategy = RecoveryStrategy::OutputTokensEscalate { old_cap, new_cap };
                    let attempt_meta = build_recovery_attempt_meta(
                        RecoveryTrigger::MaxOutputTokens,
                        &strategy,
                        &format!("stop_reason={}", response.stop_reason),
                        1,
                    );
                    self.emit_event_with_meta(
                        agent_id,
                        step,
                        "recovery_attempt",
                        &format_attempt_content(RecoveryTrigger::MaxOutputTokens, &strategy),
                        Some(attempt_meta),
                    );
                    pending_recovery_to_resolve = Some((
                        RecoveryTrigger::MaxOutputTokens,
                        RecoveryStrategy::OutputTokensEscalate { old_cap, new_cap },
                    ));

                    // **关键**：刚才 push 进 messages 末尾的 partial assistant message
                    // 必须 pop——升档相当于重发同一 prompt，旧 partial 不能留（否则
                    // LLM 会看到自己半截输出 + 同样 prompt，行为不确定）。
                    if let Some(last) = messages.last() {
                        if matches!(last.role, MessageRole::Assistant) {
                            messages.pop();
                        }
                    }
                    tracing::warn!(
                        agent_id = %agent_id,
                        step,
                        old_cap,
                        new_cap,
                        "max_output_tokens escalation; popping partial assistant + retry same step"
                    );

                    // 不 step++，复用同 step 号重发——和 reactive compact 同语义。
                    skip_step_increment_for_reactive = true;
                    continue;
                }

                // ② multi-turn recovery：可以继续接着写
                if multi_turn_recovery_count < MAX_OUTPUT_TOKENS_RECOVERY_LIMIT {
                    multi_turn_recovery_count += 1;
                    let recovery_msg = format!(
                        "[System] Output token limit ({current_cap}) hit again. \
                         Resume directly — no apology, no recap. Pick up mid-thought if that's \
                         where the cut happened. **Break remaining work into smaller pieces** \
                         (separate tool calls / shorter writes). \
                         Recovery attempt {n}/{limit}.",
                        current_cap = current_max_output_tokens,
                        n = multi_turn_recovery_count,
                        limit = MAX_OUTPUT_TOKENS_RECOVERY_LIMIT,
                    );
                    self.emit_event_with_meta(
                        agent_id,
                        step,
                        "system_hint",
                        &format!(
                            "Output truncated at {} tokens (recovery {}/{}); asked agent to resume mid-thought",
                            current_max_output_tokens,
                            multi_turn_recovery_count,
                            MAX_OUTPUT_TOKENS_RECOVERY_LIMIT,
                        ),
                        Some(serde_json::json!({
                            "kind": "max_tokens_multi_turn",
                            "max_tokens": current_max_output_tokens,
                            "stop_reason": response.stop_reason,
                            "recovery_count": multi_turn_recovery_count,
                            "recovery_limit": MAX_OUTPUT_TOKENS_RECOVERY_LIMIT,
                        })),
                    );
                    // P0-3: 双发 silent recovery_attempt
                    let strategy = RecoveryStrategy::OutputTokensContinue {
                        recovery_count: multi_turn_recovery_count,
                        recovery_limit: MAX_OUTPUT_TOKENS_RECOVERY_LIMIT,
                    };
                    let attempt_meta = build_recovery_attempt_meta(
                        RecoveryTrigger::MaxOutputTokens,
                        &strategy,
                        &format!(
                            "stop_reason={}, cap={}",
                            response.stop_reason, current_max_output_tokens
                        ),
                        multi_turn_recovery_count,
                    );
                    self.emit_event_with_meta(
                        agent_id,
                        step,
                        "recovery_attempt",
                        &format_attempt_content(RecoveryTrigger::MaxOutputTokens, &strategy),
                        Some(attempt_meta),
                    );
                    pending_recovery_to_resolve = Some((
                        RecoveryTrigger::MaxOutputTokens,
                        RecoveryStrategy::OutputTokensContinue {
                            recovery_count: multi_turn_recovery_count,
                            recovery_limit: MAX_OUTPUT_TOKENS_RECOVERY_LIMIT,
                        },
                    ));
                    if tool_use_blocks.is_empty() {
                        messages.push(Message {
                            role: MessageRole::User,
                            content: vec![ContentBlock::Text { text: recovery_msg }],
                            cache_control: None,
                        });
                    } else {
                        pending_max_tokens_hint = Some(recovery_msg);
                    }
                } else {
                    // ③ surface：multi-turn 用满了——任务本身设计有问题
                    let surface_msg = format!(
                        "[System][Persistent Error] Hit max_output_tokens after escalation \
                         to {esc} and {limit} multi-turn recoveries. The task is too large for \
                         the model in one session. **Last resort**: split the remaining work \
                         into smaller tool calls (one section per call); if the artifact you \
                         were producing must be one file, write a skeleton first then edit_file \
                         in chunks. Calling task_complete with partial progress is acceptable.",
                        esc = ESCALATED_MAX_OUTPUT_TOKENS,
                        limit = MAX_OUTPUT_TOKENS_RECOVERY_LIMIT,
                    );
                    self.emit_event_with_meta(
                        agent_id,
                        step,
                        "error",
                        &format!(
                            "max_output_tokens exhausted: escalated to {esc} + {limit} recovery attempts",
                            esc = ESCALATED_MAX_OUTPUT_TOKENS,
                            limit = MAX_OUTPUT_TOKENS_RECOVERY_LIMIT,
                        ),
                        Some(serde_json::json!({
                            "kind": "max_tokens_exhausted",
                            "escalated_cap": ESCALATED_MAX_OUTPUT_TOKENS,
                            "recovery_attempts": multi_turn_recovery_count,
                        })),
                    );
                    // 不 fail，继续走 follow-up——让 LLM 看到 surface_msg 自己决定下一步
                    if tool_use_blocks.is_empty() {
                        messages.push(Message {
                            role: MessageRole::User,
                            content: vec![ContentBlock::Text { text: surface_msg }],
                            cache_control: None,
                        });
                    } else {
                        pending_max_tokens_hint = Some(surface_msg);
                    }
                }
            }

            // Single-Agent Uplift P0-2: budget tracker 决策——是否该让 agent 收尾了。
            //
            // 触发路径与 max_tokens hint 同源（注入到 follow-up 或独立 user msg），
            // 因为本质都是"让 LLM 下一轮看到一条系统提示并相应调整"。区别：max_tokens
            // 是 protocol 级硬错误，budget 是软提醒，所以 budget nudge **不**注入
            // pending_max_tokens_hint（避免和真撞顶提示混淆）；走单独路径。
            //
            // 不变量：单 agent 只 nudge 一次（tracker.nudge_already_emitted 守住）。
            // 一次 nudge 后 LLM 通常会 task_complete；如果它装聋不 complete，max_steps
            // 还是会兜底 fail。
            let mut pending_budget_nudge: Option<String> = None;
            if let (Some(tracker), Some(budget)) =
                (budget_tracker.as_mut(), opts.output_token_budget)
            {
                let decision = tracker.decide(budget);
                if let BudgetDecision::Stop {
                    reason,
                    accumulated,
                    budget: b,
                    pct,
                } = decision
                {
                    if !tracker.nudge_already_emitted() {
                        tracker.mark_nudge_emitted();
                        let reason_str = reason.as_str();
                        let nudge = format!(
                            "[System] You have used {accumulated} output tokens out of your {b} \
                             token budget ({pct_pct:.0}% — trigger: {reason_str}). \
                             **Stop exploring** and finalize now. If the task requires output files, \
                             write best-effort valid artifacts using the evidence already collected, \
                             then call `task_complete`. If the task isn't fully done, summarise what's \
                             done and what's not — do not start new work in this turn.",
                            pct_pct = pct * 100.0,
                        );
                        self.emit_event_with_meta(
                            agent_id,
                            step,
                            "system_hint",
                            &format!(
                                "Token budget {reason_str}: {accumulated}/{b} ({:.0}%); \
                                 asked agent to wrap up via task_complete",
                                pct * 100.0,
                            ),
                            Some(serde_json::json!({
                                "kind": "budget_stop_nudge",
                                "reason": reason_str,
                                "accumulated_tokens": accumulated,
                                "budget": b,
                                "pct": pct,
                            })),
                        );
                        if tool_use_blocks.is_empty() {
                            messages.push(Message {
                                role: MessageRole::User,
                                content: vec![ContentBlock::Text { text: nudge }],
                                cache_control: None,
                            });
                        } else {
                            pending_budget_nudge = Some(nudge);
                        }
                        // log diminishing/exhausted 触发量级，便于事后调阈值
                        tracing::warn!(
                            agent_id = %agent_id,
                            step,
                            accumulated_tokens = accumulated,
                            budget = b,
                            pct = pct * 100.0,
                            reason = reason_str,
                            "token budget stop nudge injected"
                        );
                    } else {
                        // 已发过 nudge，第二次以上的 Stop 只 trace，不发事件
                        tracing::debug!(
                            agent_id = %agent_id,
                            step,
                            accumulated_tokens = accumulated,
                            "budget tracker stop re-triggered after nudge; suppressed"
                        );
                    }
                }
            }
            let task_complete_call = tool_use_blocks
                .iter()
                .find(|(_, name, _)| name == TASK_COMPLETE_TOOL);

            if let Some((_, _, input)) = task_complete_call.cloned() {
                // FR-09.3-5: 跑 guardrails，决定是 Completed 还是注入失败重试
                let summary = input
                    .get("summary")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                let summary_for_event =
                    char_safe_excerpt(&summary, TASK_COMPLETE_EVENT_SUMMARY_CHARS);
                self.emit_event_with_meta(
                    agent_id,
                    step,
                    "tool_use",
                    &format!("task_complete({{\"summary\": ...}})"),
                    Some(serde_json::json!({
                        "tool": TASK_COMPLETE_TOOL,
                        "input": {
                            "summary": summary_for_event,
                            "summary_chars": summary.chars().count(),
                            "summary_truncated": summary.chars().count() > TASK_COMPLETE_EVENT_SUMMARY_CHARS,
                        },
                    })),
                );

                // P2-1 Phase B: Stop hook 调用点。task_complete 触发的"自然 turn 结束"
                // 是用户最常注册 hook 的位置（"提交前跑 npm test"）。在 guardrail
                // evaluator 之前调，让 hook 也能 InjectMessage 让 agent 重做。
                {
                    let hook_ctx = self.build_hook_context(
                        agent_id,
                        step,
                        crate::agent::hooks::HookPhase::Stop,
                        &messages,
                        None,
                        Some(summary.clone()),
                    );
                    match self
                        .dispatch_hook_phase(agent_id, step, hook_ctx, &mut messages)
                        .await
                    {
                        Ok(()) => {}
                        Err(HookFatal::Terminal(reason)) => {
                            let msg = format!("Stop hook terminated agent: {reason}");
                            self.mark_task_failed_with_reason(agent_id, "failed", &msg);
                            self.emit_event(agent_id, step, "status_change", "failed");
                            self.update_agent_status(agent_id, "failed");
                            return Ok(AgentStatus::Failed);
                        }
                        Err(HookFatal::StepAborted(_)) => {
                            // Stop hook 的 StepAborted = "别 complete，继续跑"——
                            // 转化为 Retry 路径让主循环重试，注入的 message 已 push 进
                            // messages 由 hook handler 完成。
                            consecutive_no_tool = 0;
                            step = step.saturating_add(1);
                            continue;
                        }
                    }
                }

                let outcome = self
                    .evaluate_completion(agent_id, step, &summary, opts)
                    .await;
                match outcome {
                    CompletionOutcome::Completed => {
                        self.emit_event(agent_id, step, "message", &summary);
                        self.persist_completion_summary(agent_id, &summary);
                        // P2-1 Phase B: TaskCompleted hook 调用点。guardrail 已 pass，
                        // status 即将变 completed。典型用途：publish artifact / send
                        // notification。在 status_change 之前调，让 terminal prevent 还能拦下。
                        {
                            let hook_ctx = self.build_hook_context(
                                agent_id,
                                step,
                                crate::agent::hooks::HookPhase::TaskCompleted,
                                &messages,
                                None,
                                Some(summary.clone()),
                            );
                            match self
                                .dispatch_hook_phase(agent_id, step, hook_ctx, &mut messages)
                                .await
                            {
                                Ok(()) => {}
                                Err(HookFatal::Terminal(reason)) => {
                                    let msg = format!(
                                        "TaskCompleted hook terminated agent after summary persisted: {reason}"
                                    );
                                    self.mark_task_failed_with_reason(agent_id, "failed", &msg);
                                    self.emit_event(agent_id, step, "status_change", "failed");
                                    self.update_agent_status(agent_id, "failed");
                                    return Ok(AgentStatus::Failed);
                                }
                                Err(HookFatal::StepAborted(_)) => {
                                    // 这里的 StepAborted 含义最微妙："guardrail 全 pass + summary
                                    // 已 persist，但 hook 反悔不让收尾"。最保守做法：当作 Failed，
                                    // 避免出现"completed_at NULL 但 status=completed"的脏态。
                                    let msg =
                                        "TaskCompleted hook requested non-terminal step abort; \
                                         treating as failed to avoid completed-but-not-finalized state"
                                            .to_string();
                                    self.mark_task_failed_with_reason(agent_id, "failed", &msg);
                                    self.emit_event(agent_id, step, "status_change", "failed");
                                    self.update_agent_status(agent_id, "failed");
                                    return Ok(AgentStatus::Failed);
                                }
                            }
                        }
                        self.persist_task_handoff_packet(agent_id, &input);
                        self.emit_event(agent_id, step, "status_change", "completed");
                        self.update_agent_status(agent_id, "completed");
                        self.expire_agent_notes(agent_id);
                        return Ok(AgentStatus::Completed);
                    }
                    CompletionOutcome::Retry { feedback } => {
                        let repair_feedback =
                            guardrail_repair_instruction(&feedback, &required_output_files);
                        if retries_left == 0 {
                            let reason = format!(
                                "guardrail: retry budget exhausted ({}); last_feedback={}",
                                opts.guardrail_retry_budget,
                                repair_feedback.chars().take(160).collect::<String>()
                            );
                            self.emit_event(agent_id, step, "error", &reason);
                            self.emit_event(agent_id, step, "status_change", "failed");
                            self.update_agent_status(agent_id, "failed");
                            self.expire_agent_notes(agent_id);
                            self.mark_task_failed_with_reason(agent_id, "failed", &reason);
                            return Ok(AgentStatus::Failed);
                        }
                        retries_left -= 1;
                        guardrail_repair_active = true;
                        last_guardrail_repair_feedback = Some(repair_feedback.clone());
                        let mut tool_results: Vec<ContentBlock> = Vec::new();
                        // 把 task_complete 工具回执填回（避免破坏 OpenAI tool_use 配对）
                        for (id, name, _) in &tool_use_blocks {
                            if name == TASK_COMPLETE_TOOL {
                                tool_results.push(ContentBlock::ToolResult {
                                    tool_use_id: id.clone(),
                                    content: repair_feedback.clone(),
                                    is_error: true,
                                });
                            }
                        }
                        // Once task_complete failed validation, make finalization terminal for this
                        // model turn. Return paired tool results for sibling tool_use blocks without
                        // executing them so special tools cannot bypass dispatch/approval paths.
                        for (id, name, _) in &tool_use_blocks {
                            if name == TASK_COMPLETE_TOOL {
                                continue;
                            }
                            tool_results.push(ContentBlock::ToolResult {
                                tool_use_id: id.clone(),
                                content: format!(
                                    "{repair_feedback}\nTool `{name}` was not run because task_complete failed contract validation in the same turn. Repair the validation failure, then call task_complete again."
                                ),
                                is_error: true,
                            });
                        }
                        messages.push(Message {
                            role: MessageRole::User,
                            content: tool_results,
                            cache_control: None,
                        });
                        consecutive_no_tool = 0;
                        continue;
                    }
                }
            }

            let has_any_tool_use = !tool_use_blocks.is_empty();
            if !has_any_tool_use {
                consecutive_no_tool += 1;
                if consecutive_no_tool >= MAX_CONSECUTIVE_NO_TOOL {
                    let last_response_had_visible_text =
                        assistant_text_from_blocks(&response.content).is_some();
                    let hint =
                        no_tool_progress_hint(consecutive_no_tool, last_response_had_visible_text);
                    messages.push(Message {
                        role: MessageRole::User,
                        content: vec![ContentBlock::Text { text: hint.clone() }],
                        cache_control: None,
                    });
                    self.emit_event(agent_id, step, "system_hint", &hint);
                }
                continue;
            }
            consecutive_no_tool = 0;

            if has_any_tool_use {
                if let Some((_, _, _)) = tool_use_blocks.iter().find(|(_, name, input)| {
                    name == "shell_exec" && shell_command_invokes_nested_agent(input)
                }) {
                    let feedback = nested_agent_feedback();
                    self.emit_event_with_meta(
                        agent_id,
                        step,
                        "system_hint",
                        &feedback,
                        Some(serde_json::json!({
                            "kind": "nested_agent_tool_block",
                            "blocked_tool": "shell_exec",
                            "blocked_tool_count": tool_use_blocks.len(),
                        })),
                    );
                    let tool_results = tool_use_blocks
                        .iter()
                        .map(|(id, name, _)| ContentBlock::ToolResult {
                            tool_use_id: id.clone(),
                            content: if name == "shell_exec" {
                                feedback.clone()
                            } else {
                                format!(
                                    "{feedback}\nTool `{name}` was not run because the same turn attempted to spawn a nested agent."
                                )
                            },
                            is_error: true,
                        })
                        .collect();
                    messages.push(Message {
                        role: MessageRole::User,
                        content: tool_results,
                        cache_control: None,
                    });
                    consecutive_no_tool = 0;
                    continue;
                }
            }

            if has_any_tool_use && !timeout_finalization_active {
                let remaining_steps = opts.max_steps.saturating_sub(step);
                if opts.max_steps > ARTIFACT_CHECKPOINT_REMAINING_STEPS
                    && remaining_steps <= ARTIFACT_CHECKPOINT_REMAINING_STEPS
                {
                    let missing_required_files = missing_required_files.clone();
                    if !missing_required_files.is_empty() {
                        let has_allowed_checkpoint_tool =
                            tool_use_blocks.iter().any(|(_, name, input)| {
                                artifact_checkpoint_allows_tool_for_remaining_steps(
                                    name,
                                    input,
                                    &missing_required_files,
                                    remaining_steps,
                                )
                            });
                        if !has_allowed_checkpoint_tool {
                            if let Some((_, blocked_name, _)) =
                                tool_use_blocks.iter().find(|(_, name, input)| {
                                    !artifact_checkpoint_allows_tool_for_remaining_steps(
                                        name,
                                        input,
                                        &missing_required_files,
                                        remaining_steps,
                                    )
                                })
                            {
                                let feedback = artifact_checkpoint_feedback(
                                    blocked_name,
                                    &missing_required_files,
                                    remaining_steps,
                                );
                                self.emit_event_with_meta(
                                    agent_id,
                                    step,
                                    "system_hint",
                                    &feedback,
                                    Some(serde_json::json!({
                                        "kind": "artifact_checkpoint_tool_block",
                                        "blocked_tool": blocked_name,
                                        "remaining_steps": remaining_steps,
                                        "missing_required_files": missing_required_files,
                                        "blocked_tool_count": tool_use_blocks.len(),
                                    })),
                                );
                                let tool_results = tool_use_blocks
                                    .iter()
                                    .map(|(id, name, _)| ContentBlock::ToolResult {
                                        tool_use_id: id.clone(),
                                        content: if name == blocked_name {
                                            feedback.clone()
                                        } else {
                                            format!(
                                                "{feedback}\nTool `{name}` was not run because required artifact checkpoint mode is active."
                                            )
                                        },
                                        is_error: false,
                                    })
                                    .collect();
                                messages.push(Message {
                                    role: MessageRole::User,
                                    content: tool_results,
                                    cache_control: None,
                                });
                                consecutive_no_tool = 0;
                                continue;
                            }
                        }
                    }
                }
            }

            if has_any_tool_use
                && guardrail_repair_active
                && opts
                    .task_contract
                    .as_ref()
                    .map(|contract| contract.completion_policy.stop_exploration_during_repair)
                    .unwrap_or(true)
            {
                if let Some((_, blocked_name, _)) = tool_use_blocks
                    .iter()
                    .find(|(_, name, input)| !finalization_allows_tool(name, input))
                {
                    let feedback = guardrail_repair_tool_block_feedback(
                        blocked_name,
                        last_guardrail_repair_feedback.as_deref(),
                    );
                    self.emit_event_with_meta(
                        agent_id,
                        step,
                        "system_hint",
                        &feedback,
                        Some(serde_json::json!({
                            "kind": "guardrail_repair_tool_block",
                            "blocked_tool": blocked_name,
                            "blocked_tool_count": tool_use_blocks.len(),
                        })),
                    );
                    let tool_results = tool_use_blocks
                        .iter()
                        .map(|(id, name, _)| ContentBlock::ToolResult {
                            tool_use_id: id.clone(),
                            content: if name == blocked_name {
                                feedback.clone()
                            } else {
                                format!(
                                    "{feedback}\nTool `{name}` was not run because late guardrail repair mode is active."
                                )
                            },
                            is_error: false,
                        })
                        .collect();
                    messages.push(Message {
                        role: MessageRole::User,
                        content: tool_results,
                        cache_control: None,
                    });
                    consecutive_no_tool = 0;
                    continue;
                }
            }

            if has_any_tool_use && timeout_finalization_active {
                let missing_required_files = missing_required_files.clone();
                let remaining_wall_secs = opts
                    .timeout_secs
                    .max(1)
                    .saturating_sub(run_started_at.elapsed().as_secs());
                if let Some((_, blocked_name, _)) = tool_use_blocks
                    .iter()
                    .find(|(_, name, input)| !finalization_allows_tool(name, input))
                {
                    let feedback = finalization_tool_block_feedback(
                        blocked_name,
                        remaining_wall_secs,
                        &missing_required_files,
                    );
                    self.emit_event_with_meta(
                        agent_id,
                        step,
                        "system_hint",
                        &feedback,
                        Some(serde_json::json!({
                            "kind": "timeout_finalization_tool_block",
                            "blocked_tool": blocked_name,
                            "remaining_wall_secs": remaining_wall_secs,
                            "missing_required_files": missing_required_files,
                            "blocked_tool_count": tool_use_blocks.len(),
                        })),
                    );
                    let tool_results = tool_use_blocks
                        .iter()
                        .map(|(id, name, _)| ContentBlock::ToolResult {
                            tool_use_id: id.clone(),
                            content: if name == blocked_name {
                                feedback.clone()
                            } else {
                                format!(
                                    "{feedback}\nTool `{name}` was not run because timeout finalization mode is active."
                                )
                            },
                            is_error: false,
                        })
                        .collect();
                    messages.push(Message {
                        role: MessageRole::User,
                        content: tool_results,
                        cache_control: None,
                    });
                    consecutive_no_tool = 0;
                    continue;
                }
            }

            if has_any_tool_use {
                let remaining_steps = opts.max_steps.saturating_sub(step);
                if let Some((_, blocked_name, _)) = tool_use_blocks
                    .iter()
                    .find(|(_, name, _)| !long_task_policy_allows_tool(name, remaining_steps))
                {
                    let feedback = long_task_policy_feedback(blocked_name, remaining_steps);
                    self.emit_event_with_meta(
                        agent_id,
                        step,
                        "system_hint",
                        &feedback,
                        Some(serde_json::json!({
                            "kind": "long_task_finalization_tool_block",
                            "blocked_tool": blocked_name,
                            "remaining_steps": remaining_steps,
                            "blocked_tool_count": tool_use_blocks.len(),
                        })),
                    );
                    let tool_results = tool_use_blocks
                        .iter()
                        .map(|(id, name, _)| ContentBlock::ToolResult {
                            tool_use_id: id.clone(),
                            content: if name == blocked_name {
                                feedback.clone()
                            } else {
                                format!(
                                    "{feedback}\nTool `{name}` was not run because finalization mode is active."
                                )
                            },
                            is_error: false,
                        })
                        .collect();
                    messages.push(Message {
                        role: MessageRole::User,
                        content: tool_results,
                        cache_control: None,
                    });
                    consecutive_no_tool = 0;
                    continue;
                }
            }

            // L3 循环检测：连续 N 步只调用只读工具（read/search/list）→ 注入"开始动手"提示，
            // 帮 LLM 跳出"光读不写"的死循环。一次性，避免重复打扰。
            let all_read_only = tool_use_blocks
                .iter()
                .all(|(_, name, input)| tool_is_read_only_loop_exploration(name, input));
            if all_read_only {
                consecutive_read_only += 1;
            } else {
                consecutive_read_only = 0;
                hinted_read_only_loop = false;
            }
            // L3 hint 不能 inline push 一条独立 user message——这一步 LLM 已经发了
            // tool_calls，紧跟着必须是 tool_results follow-up。把 hint 延迟到 follow-up
            // message 里再 append，避免触发 DeepSeek/OpenAI 协议层 400
            // (insufficient tool messages following tool_calls message)。
            //
            // 详见 [`ToolFollowupBuilder`] 文档。
            let pending_read_only_hint =
                if !hinted_read_only_loop && consecutive_read_only >= READ_ONLY_LOOP_THRESHOLD {
                    hinted_read_only_loop = true;
                    let hint = format!(
                    "[System] You have spent {} consecutive steps only reading / searching files \
                     without making any change. Either start writing (`write_file`), running a \
                     command (`shell_exec`), or — if exploration is finished — call \
                     `task_complete`. Endless exploration is treated as a failure.",
                    consecutive_read_only
                );
                    self.emit_event(agent_id, step, "system_hint", &hint);
                    Some(hint)
                } else {
                    None
                };

            if has_any_tool_use
                && !missing_required_files.is_empty()
                && consecutive_read_only >= ARTIFACT_FIRST_HINT_STEP
            {
                if let Some((_, blocked_name, _)) = tool_use_blocks
                    .iter()
                    .find(|(_, name, input)| tool_is_read_only_loop_exploration(name, input))
                {
                    let feedback =
                        read_only_loop_block_feedback(blocked_name, &missing_required_files);
                    self.emit_event_with_meta(
                        agent_id,
                        step,
                        "system_hint",
                        &feedback,
                        Some(serde_json::json!({
                            "kind": "artifact_missing_read_only_loop_tool_block",
                            "blocked_tool": blocked_name,
                            "consecutive_read_only": consecutive_read_only,
                            "missing_required_files": missing_required_files,
                            "blocked_tool_count": tool_use_blocks.len(),
                        })),
                    );
                    let tool_results = tool_use_blocks
                        .iter()
                        .map(|(id, name, _)| ContentBlock::ToolResult {
                            tool_use_id: id.clone(),
                            content: if name == blocked_name {
                                feedback.clone()
                            } else {
                                format!(
                                    "{feedback}\nTool `{name}` was not run because required artifacts are still missing and artifact-first recovery mode is active."
                                )
                            },
                            is_error: false,
                        })
                        .collect();
                    messages.push(Message {
                        role: MessageRole::User,
                        content: tool_results,
                        cache_control: None,
                    });
                    consecutive_no_tool = 0;
                    continue;
                }
            }

            if has_any_tool_use && consecutive_read_only >= READ_ONLY_LOOP_THRESHOLD {
                let missing_required_files = missing_required_files.clone();
                if missing_required_files.is_empty()
                    && required_output_files.is_empty()
                    && has_direct_response_contract(opts)
                {
                    if let Some((_, blocked_name, _)) = tool_use_blocks
                        .iter()
                        .find(|(_, name, input)| tool_is_read_only_loop_exploration(name, input))
                    {
                        let feedback = direct_output_loop_block_feedback(blocked_name);
                        self.emit_event_with_meta(
                            agent_id,
                            step,
                            "system_hint",
                            &feedback,
                            Some(serde_json::json!({
                                "kind": "direct_output_read_loop_tool_block",
                                "blocked_tool": blocked_name,
                                "consecutive_read_only": consecutive_read_only,
                                "blocked_tool_count": tool_use_blocks.len(),
                            })),
                        );
                        let tool_results = tool_use_blocks
                            .iter()
                            .map(|(id, name, _)| ContentBlock::ToolResult {
                                tool_use_id: id.clone(),
                                content: if name == blocked_name {
                                    feedback.clone()
                                } else {
                                    format!(
                                        "{feedback}\nTool `{name}` was not run because direct-response finalization mode is active."
                                    )
                                },
                                is_error: false,
                            })
                            .collect();
                        messages.push(Message {
                            role: MessageRole::User,
                            content: tool_results,
                            cache_control: None,
                        });
                        consecutive_no_tool = 0;
                        continue;
                    }
                } else if !missing_required_files.is_empty() {
                    if let Some((_, blocked_name, _)) = tool_use_blocks
                        .iter()
                        .find(|(_, name, input)| !read_only_loop_allows_tool(name, input))
                    {
                        let feedback =
                            read_only_loop_block_feedback(blocked_name, &missing_required_files);
                        self.emit_event_with_meta(
                            agent_id,
                            step,
                            "system_hint",
                            &feedback,
                            Some(serde_json::json!({
                                "kind": "read_only_loop_tool_block",
                                "blocked_tool": blocked_name,
                                "consecutive_read_only": consecutive_read_only,
                                "missing_required_files": missing_required_files,
                                "blocked_tool_count": tool_use_blocks.len(),
                            })),
                        );
                        let tool_results = tool_use_blocks
                            .iter()
                            .map(|(id, name, _)| ContentBlock::ToolResult {
                                tool_use_id: id.clone(),
                                content: if name == blocked_name {
                                    feedback.clone()
                                } else {
                                    format!(
                                        "{feedback}\nTool `{name}` was not run because read-only loop recovery mode is active."
                                    )
                                },
                                is_error: false,
                            })
                            .collect();
                        messages.push(Message {
                            role: MessageRole::User,
                            content: tool_results,
                            cache_control: None,
                        });
                        consecutive_no_tool = 0;
                        continue;
                    }
                }
            }

            // Single-Agent Uplift Phase 2.1: 并发安全的工具批量并行执行。
            //
            // 之前所有 tool_use 严格串行 → 一个 step 跑 3 个 read_file 等于 3× IO 延迟。
            // 现在按 ToolSpec.is_concurrency_safe 分桶：
            //   - safe  (read_file / list_files / grep / glob): 并行跑
            //   - unsafe(write_file / edit_file / shell_exec / publish_artifact /
            //            todo_write): 串行跑（防写盘冲突 / approval gate 顺序错乱）
            //
            // tool_use 事件全部前置一次性 emit，让用户立即看到"这一批要做这些"；
            // tool_result 事件在每个 future 完成时 emit（顺序按完成时间，不按 tool_use_blocks
            // 顺序），用户能感知到"X 已经回来了，Y 还在跑"。
            //
            // tool_results vec 仍按原顺序填回 messages，因为 Anthropic 要求 ToolResult
            // 序与同 turn 的 ToolUse 严格一一对应。
            //
            // 注意：approval_gate 只拦截 unsafe 工具（write 类 / shell 类），所以 safe
            // 桶不会因为 approval 等待造成相互阻塞 → 真正能并行。
            for (id, name, input) in &tool_use_blocks {
                let tool_use_meta = serde_json::json!({
                    "tool": name,
                    "tool_use_id": id,
                    "input": input,
                });
                self.emit_event_with_meta(
                    agent_id,
                    step,
                    "tool_use",
                    &format!(
                        "{name}({})",
                        serde_json::to_string(input).unwrap_or_default()
                    ),
                    Some(tool_use_meta),
                );
            }

            // 计算每个 block 是否 concurrency-safe。未知工具（registry 没注册的）
            // 默认按 unsafe 处理——保守。task_complete / publish_artifact / todo_write
            // 不在 registry，自然落 unsafe。
            let safe_flags: Vec<bool> = tool_use_blocks
                .iter()
                .map(|(_, name, _)| {
                    crate::tools::lookup_tool_spec(name)
                        .map(|s| s.is_concurrency_safe)
                        .unwrap_or(false)
                })
                .collect();

            // 预分配结果 slot；按原 index 填回。Vec<Option<...>> 是常见的"并行下沉
            // 后保持顺序"模式，比 HashMap 省一次哈希且 cache friendly。
            let mut tool_outputs: Vec<Option<crate::tools::ToolOutput>> =
                (0..tool_use_blocks.len()).map(|_| None).collect();

            // 1) 并行跑 safe 桶。futures::future::join_all 不要求 'static，
            //    每个 future 借 &self，到 .await 结束借用归还。
            let safe_futures: Vec<_> = tool_use_blocks
                .iter()
                .enumerate()
                .filter_map(|(i, blk)| {
                    if safe_flags[i] {
                        let (id, name, input) = blk;
                        Some(async move {
                            let started_at = std::time::Instant::now();
                            let output = self.dispatch_tool(agent_id, step, id, name, input).await;
                            let duration_ms = started_at.elapsed().as_millis() as u64;
                            (i, id.clone(), name.clone(), output, duration_ms)
                        })
                    } else {
                        None
                    }
                })
                .collect();
            if !safe_futures.is_empty() {
                let results = futures::future::join_all(safe_futures).await;
                for (i, id, name, output, duration_ms) in results {
                    let event_kind = if output.is_error {
                        "error"
                    } else {
                        "tool_result"
                    };
                    let mut result_meta = serde_json::json!({
                        "tool": name,
                        "tool_use_id": id,
                        "is_error": output.is_error,
                        "duration_ms": duration_ms,
                        "size_chars": output.content.chars().count(),
                        "concurrent": true,
                    });
                    if let Some(extra_meta) = output.meta.clone() {
                        result_meta["output"] = extra_meta;
                    }
                    self.emit_event_with_meta(
                        agent_id,
                        step,
                        event_kind,
                        &output.content,
                        Some(result_meta),
                    );
                    tool_outputs[i] = Some(output);
                }
            }

            // 2) 串行跑 unsafe 桶。保留原 index 顺序。
            for (i, blk) in tool_use_blocks.iter().enumerate() {
                if safe_flags[i] {
                    continue;
                }
                let (id, name, input) = blk;
                let started_at = std::time::Instant::now();
                let output = self.dispatch_tool(agent_id, step, id, name, input).await;
                let duration_ms = started_at.elapsed().as_millis() as u64;
                let event_kind = if output.is_error {
                    "error"
                } else {
                    "tool_result"
                };
                let mut result_meta = serde_json::json!({
                    "tool": name,
                    "tool_use_id": id,
                    "is_error": output.is_error,
                    "duration_ms": duration_ms,
                    "size_chars": output.content.chars().count(),
                    "concurrent": false,
                });
                if let Some(extra_meta) = output.meta.clone() {
                    result_meta["output"] = extra_meta;
                }
                self.emit_event_with_meta(
                    agent_id,
                    step,
                    event_kind,
                    &output.content,
                    Some(result_meta),
                );
                tool_outputs[i] = Some(output);
            }

            // 3) 按原顺序拼 tool_results 到 follow-up builder。Anthropic 要求 ToolResult
            //    与 ToolUse 同 turn 严格按 tool_use_id 配对——顺序错了 API 会 400。
            let pending_invalid_args_hint = invalid_tool_args_recovery_state.observe_tool_batch(
                &tool_use_blocks,
                &tool_outputs,
                &missing_required_files,
            );
            if let Some(hint) = pending_invalid_args_hint.as_ref() {
                self.emit_event_with_meta(
                    agent_id,
                    step,
                    "system_hint",
                    hint,
                    Some(serde_json::json!({
                        "kind": "invalid_tool_args_recovery",
                        "tool": "write_file",
                        "malformed_write_file_count": invalid_tool_args_recovery_state.malformed_write_file_count,
                        "missing_required_files": missing_required_files,
                    })),
                );
            }
            let mut followup = ToolFollowupBuilder::with_capacity(tool_use_blocks.len());
            let rendered_tool_results = {
                let context_inputs = tool_use_blocks
                    .iter()
                    .zip(tool_outputs.iter())
                    .map(|((id, name, input), output_opt)| {
                        let output = output_opt
                            .as_ref()
                            .expect("dispatch_tool 必须为每个 tool_use_block 填回一个 ToolOutput");
                        crate::agent::tool_result_policy::ToolResultContextInput {
                            step,
                            tool_use_id: id,
                            tool_name: name,
                            input,
                            output,
                        }
                    })
                    .collect::<Vec<_>>();
                crate::agent::tool_result_policy::ToolResultContextPolicy::apply_batch(
                    &mut tool_result_context_state,
                    &context_inputs,
                )
            };

            for (((id, _, _), output_opt), rendered) in tool_use_blocks
                .iter()
                .zip(tool_outputs.into_iter())
                .zip(rendered_tool_results.into_iter())
            {
                let output = output_opt
                    .expect("dispatch_tool 必须为每个 tool_use_block 填回一个 ToolOutput");
                if let Some(report) = rendered.report {
                    if report.mode != crate::agent::tool_result_policy::ContextRenderMode::Inline
                        || report.per_message_budget_replaced
                    {
                        let context_policy_meta = serde_json::json!({
                            "mode": report.mode.as_str(),
                            "original_chars": report.original_chars,
                            "context_chars": report.context_chars,
                            "saved_chars": report.saved_chars,
                            "fingerprint": report.content_fingerprint,
                            "source_fingerprint": report.source_fingerprint,
                            "repeat_of": report.repeat_of,
                            "evidence_id": report.evidence_path,
                            "persisted_path": report.persisted_path,
                            "per_message_budget_replaced": report.per_message_budget_replaced,
                        });
                        self.emit_event_with_meta(
                            agent_id,
                            step,
                            "tool_result_policy",
                            &format!(
                                "Rendered {} tool_result as {}: {} chars → {} chars",
                                report.tool_name,
                                report.mode.as_str(),
                                report.original_chars,
                                report.context_chars
                            ),
                            Some(serde_json::json!({
                                "tool": report.tool_name,
                                "tool_use_id": report.tool_use_id,
                                "from_chars": report.original_chars,
                                "to_chars": report.context_chars,
                                "saved_chars": report.saved_chars,
                                "reason": report.reason,
                                "context_policy": context_policy_meta,
                            })),
                        );
                    }
                }
                followup.push_tool_result(id.clone(), rendered.content, output.is_error);
            }

            // max_tokens-hit hint（仅当本步有 tool_calls 时才暂存到此）：
            // 同样必须并入 follow-up，避免 [tool_calls][user_text][tool_results] 协议违例。
            if let Some(hint) = pending_max_tokens_hint {
                followup.append_hint(hint);
            }

            // P0-2: budget tracker 触发的"该收尾了" nudge（仅当本步有 tool_calls 时
            // 才暂存到此）。同 max_tokens hint 走相同协议路径——OpenAI tool_call 紧跟
            // tool_result 配对要求。
            if let Some(nudge) = pending_budget_nudge {
                followup.append_hint(nudge);
            }

            // L3 read-only-loop hint：必须叠在同一条 follow-up message 里，
            // 不能拆成独立的 user text message——见 [`ToolFollowupBuilder`] 协议说明。
            if let Some(hint) = pending_read_only_hint {
                followup.append_hint(hint);
            }

            // Malformed tool-argument recovery also has to be appended to the same follow-up
            // message, because the assistant turn already contains tool_use blocks.
            if let Some(hint) = pending_invalid_args_hint {
                followup.append_hint(hint);
            }

            // 处理 directive notes —— 同样作为 follow-up hint 叠加。
            let queued_notes = self.poll_queued_notes(agent_id);
            if !queued_notes.is_empty() {
                let notes_text = Self::format_notes_for_injection(&queued_notes);
                let note_ids: Vec<String> = queued_notes.iter().map(|(id, _)| id.clone()).collect();
                self.mark_notes_applied(&note_ids);
                // 走 emit_event_with_meta 可以把 note 实际内容也带过去，让前端把 directive
                // 高亮渲染（之前 only ack 行为，content 太干）。
                let notes_meta = serde_json::json!({
                    "applied_count": queued_notes.len(),
                    "note_ids": note_ids,
                    "notes": queued_notes.iter().map(|(_, c)| c.clone()).collect::<Vec<_>>(),
                });
                self.emit_event_with_meta(
                    agent_id,
                    step,
                    "note_applied",
                    &format!("Applied {} note(s)", queued_notes.len()),
                    Some(notes_meta),
                );
                followup.append_hint(notes_text);
            }

            messages.push(followup.build());

            // P2-1 Phase B: PostToolUse hook 调用点。批量执行后触发一次（而非每个 tool
            // 一次），avoid 一个 step 多个 tool 时事件淹没。hook 想"逐工具"信息可以从
            // `ctx.messages_summary.recent_tool_uses` 倒序拿到本批次所有工具名。
            // last_tool_use 取本批次最后一个有 ToolUse + ToolResult 配对的工具——
            // 简化实现：取 tool_use_blocks 的最后一个 + 它的输出（来自刚 push 的 followup
            // message 的最后一个 ToolResult block）。
            if !tool_use_blocks.is_empty() {
                let (last_id, last_name, last_input) = tool_use_blocks
                    .last()
                    .expect("non-empty guarded above")
                    .clone();
                // 从刚 push 的 followup message 倒序找匹配的 ToolResult
                let (last_output_excerpt, last_is_error) = messages
                    .last()
                    .and_then(|m| {
                        m.content.iter().rev().find_map(|b| match b {
                            ContentBlock::ToolResult {
                                tool_use_id,
                                content,
                                is_error,
                            } if tool_use_id == &last_id => {
                                let excerpt: String = content.chars().take(2048).collect();
                                Some((excerpt, *is_error))
                            }
                            _ => None,
                        })
                    })
                    .unwrap_or_else(|| (String::new(), false));
                let hook_ctx = self.build_hook_context(
                    agent_id,
                    step,
                    crate::agent::hooks::HookPhase::PostToolUse,
                    &messages,
                    Some(crate::agent::hooks::HookToolUseInfo {
                        tool_use_id: last_id,
                        tool_name: last_name,
                        input: last_input,
                        output_excerpt: last_output_excerpt,
                        is_error: last_is_error,
                    }),
                    None,
                );
                match self
                    .dispatch_hook_phase(agent_id, step, hook_ctx, &mut messages)
                    .await
                {
                    Ok(()) => {}
                    Err(HookFatal::Terminal(reason)) => {
                        let msg = format!("PostToolUse hook terminated agent: {reason}");
                        self.mark_task_failed_with_reason(agent_id, "failed", &msg);
                        self.emit_event(agent_id, step, "status_change", "failed");
                        self.update_agent_status(agent_id, "failed");
                        return Ok(AgentStatus::Failed);
                    }
                    Err(HookFatal::StepAborted(_)) => {
                        // 不终止 agent，但跳到下一 step（hook 注入的 message 已 push）
                        consecutive_no_tool = 0;
                        step = step.saturating_add(1);
                        continue;
                    }
                }
            }

            if self.cancel_token.is_cancelled() {
                return self.finish_cancelled(agent_id, step);
            }
        }
    }

    /// Single-Agent Uplift B2: tool_result 瘦身的统一入口，先摘要再 truncate。
    ///
    /// 行为：
    ///   1. 扫所有 ToolResult content，挑出 chars > tool_summary_threshold_chars 的
    ///   2. 如果 tool_summarizer 已配置 → 并发请求摘要，成功则原文替换为 `[summary] ...`
    ///      并 emit `tool_summary` 事件供前端展示
    ///   3. 仍然超阈值（摘要失败 / 未配置）→ fallback 到 apply_tool_result_budget 的截尾逻辑
    ///   4. 摘要本身也走截尾兜底 —— 防止小模型不听话吐了 5K 字
    async fn apply_tool_result_budget_with_optional_summary(
        &self,
        agent_id: &str,
        step: u32,
        messages: &mut [Message],
    ) {
        let compacted_tool_use_inputs = compact_large_tool_use_inputs(messages);
        if compacted_tool_use_inputs > 0 {
            self.emit_event_with_meta(
                agent_id,
                step,
                "compact",
                &format!(
                    "Compacted {compacted_tool_use_inputs} completed tool_use input(s) to keep context lean."
                ),
                Some(serde_json::json!({
                    "kind": "tool_use_input",
                    "compacted_tool_use_inputs": compacted_tool_use_inputs,
                })),
            );
        }

        let policy_reports = crate::agent::tool_result_policy::apply_policy_to_messages(messages);
        for report in policy_reports {
            self.emit_event_with_meta(
                agent_id,
                step,
                "tool_result_policy",
                &format!(
                    "Compacted {} tool_result: {} chars → {} chars",
                    report.tool_name, report.original_chars, report.compacted_chars
                ),
                Some(serde_json::json!({
                    "tool": report.tool_name,
                    "tool_use_id": report.tool_use_id,
                    "from_chars": report.original_chars,
                    "to_chars": report.compacted_chars,
                    "reason": report.reason,
                })),
            );
        }

        if let Some(summarizer) = &self.tool_summarizer {
            let tool_lookup = crate::agent::tool_result_policy::tool_lookup_from_messages(messages);
            // 收集需要摘要的 (msg_idx, block_idx, tool_name, content) 清单。
            // 仅对 chars > threshold 的 content 走小模型，避免大量小 result 拖慢主循环。
            let mut targets: Vec<(usize, usize, String, String)> = Vec::new();
            for (mi, msg) in messages.iter().enumerate() {
                for (bi, block) in msg.content.iter().enumerate() {
                    if let ContentBlock::ToolResult {
                        content,
                        tool_use_id,
                        ..
                    } = block
                    {
                        if crate::agent::tool_result_policy::is_already_compacted(content) {
                            continue;
                        }
                        if content.chars().count() <= self.tool_summary_threshold_chars {
                            continue;
                        }
                        let tool_name = tool_lookup
                            .get(tool_use_id)
                            .map(|(name, _)| name.clone())
                            .unwrap_or_else(|| tool_use_id.clone());
                        targets.push((mi, bi, tool_name, content.clone()));
                    }
                }
            }

            if !targets.is_empty() {
                let futures = targets.iter().map(|(_, _, tool, content)| {
                    let summarizer = summarizer.clone();
                    let tool = tool.clone();
                    let content = content.clone();
                    async move {
                        let started = std::time::Instant::now();
                        let res = summarizer.summarize(&tool, &content).await;
                        (res, started.elapsed())
                    }
                });
                let results = futures::future::join_all(futures).await;

                for ((mi, bi, tool_label, original), (res, dur)) in
                    targets.iter().zip(results.into_iter())
                {
                    let original_chars = original.chars().count();
                    match res {
                        Ok(summary) => {
                            // 兜底截尾，防摘要本身过长（小模型偶尔不听话）
                            const SUMMARY_HARD_CAP: usize = 1500;
                            let summary_trimmed: String =
                                if summary.chars().count() > SUMMARY_HARD_CAP {
                                    summary.chars().take(SUMMARY_HARD_CAP).collect::<String>()
                                        + "…[summary truncated]"
                                } else {
                                    summary
                                };
                            let summary_chars = summary_trimmed.chars().count();
                            let replacement = format!(
                                "[tool_summary] (orig {}KB → ~{} chars; full output preserved in agent_events)\n{}",
                                original_chars / 1024,
                                summary_chars,
                                summary_trimmed,
                            );
                            if let ContentBlock::ToolResult { content, .. } =
                                &mut messages[*mi].content[*bi]
                            {
                                *content = replacement;
                            }
                            self.emit_event_with_meta(
                                agent_id,
                                step,
                                "tool_summary",
                                &format!(
                                    "Summarized large tool_result: {} chars → {} chars ({} ms)",
                                    original_chars,
                                    summary_chars,
                                    dur.as_millis()
                                ),
                                Some(serde_json::json!({
                                    "tool": tool_label,
                                    "from_chars": original_chars,
                                    "to_chars": summary_chars,
                                    "duration_ms": dur.as_millis() as u64,
                                    "model": self.tool_summarizer.as_ref().map(|s| s.model().to_string()),
                                })),
                            );
                        }
                        Err(e) => {
                            tracing::warn!(
                                "tool_summary failed for tool_use_id={}: {} — falling back to truncate",
                                tool_label,
                                e
                            );
                            // 不动 messages；下一步 apply_tool_result_budget 会做 truncate fallback
                        }
                    }
                }
            }
        }

        // 不论摘要是否启用都跑一遍 truncate：
        // - 摘要成功 → content 已是 [tool_summary] 短串，starts_with sentinel，直接跳过
        // - 摘要失败 / 未启用 → 在这里截尾兜底
        apply_tool_result_budget(messages);
    }

    /// 派发工具：`publish_artifact` 由 artifacts 模块直接处理（需要 DB），其它走 ToolExecutor。
    /// `task_complete` 已经在主循环里被截断，这里不会进来。
    ///
    /// FM-14：在真正执行前先过 approval_gate.maybe_intercept_tool；命中策略且用户拒绝，
    /// 则用一个 is_error=true 的 ToolOutput 直接替代结果，让 LLM 自然走"换种方式"路径。
    async fn dispatch_tool(
        &self,
        agent_id: &str,
        step: u32,
        tool_use_id: &str,
        name: &str,
        input: &serde_json::Value,
    ) -> crate::tools::ToolOutput {
        let dispatch_started = std::time::Instant::now();
        // input 直接 to_string 可能很大（write_file content）；只截 200 字给日志，
        // 真正完整 input 还是在 agent-event payload 里有，用户回看可以拿到。
        let input_excerpt = {
            let s = serde_json::to_string(input).unwrap_or_default();
            char_safe_excerpt(&s, 200)
        };
        tracing::info!(
            agent_id = %agent_id,
            tool = %name,
            input_len = input_excerpt.len(),
            input_excerpt = %input_excerpt,
            "tool_dispatch begin"
        );

        let result = self
            .dispatch_tool_inner(agent_id, step, tool_use_id, name, input, dispatch_started)
            .await;

        tracing::info!(
            agent_id = %agent_id,
            tool = %name,
            duration_ms = dispatch_started.elapsed().as_millis() as u64,
            is_error = result.is_error,
            output_len = result.content.len(),
            "tool_dispatch done"
        );
        result
    }

    /// dispatch_tool 的实现体；分离出来只是为了让 dispatch_tool 包一层
    /// "进/出 trace + 计时"，避免每个 return 分支都要复制日志代码。
    async fn dispatch_tool_inner(
        &self,
        agent_id: &str,
        step: u32,
        tool_use_id: &str,
        name: &str,
        input: &serde_json::Value,
        _started: std::time::Instant,
    ) -> crate::tools::ToolOutput {
        // Single-Agent Uplift: 兜底解释 LLM 漏写 arguments 的情况。
        // OpenAI-compat provider 在 args 字符串为空 / parse 失败时会塞 sentinel 进 input，
        // 这里识别后给 LLM 一个**明确**的错误，让它理解是自己漏给参数（而不是 schema 错）。
        if let Some(obj) = input.as_object() {
            if let Some(err) = obj
                .get(crate::llm::ARG_PARSE_ERROR_KEY)
                .and_then(|v| v.as_str())
            {
                let raw = obj
                    .get(crate::llm::ARG_RAW_KEY)
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let raw_excerpt = char_safe_excerpt(raw, 400);
                let retry_guidance = if name == "write_file" {
                    "Do not retry the same large write_file payload. On the next turn choose one of these recovery paths: (1) write a minimal complete, executable artifact generator in small append chunks under ~2KB each; (2) if only a small fix is needed, append exactly one small chunk; or (3) run an already-written local generator with shell_exec. Prefer a complete but simpler artifact over a large polished script that may truncate again."
                } else if matches!(name, "edit_file" | "shell_exec") {
                    "Do not retry the same large payload. Use a shorter command/patch, split large writes into small write_file append chunks under ~2KB, or create a minimal complete artifact first and refine only if steps remain."
                } else {
                    "Retry the call with all required parameters spelled out explicitly."
                };
                let msg = format!(
                    "tool_use for `{name}` arrived without valid arguments ({err}). \
                     Raw arguments string from the model: {:?}. \
                     Likely cause: the previous response hit max_tokens before the JSON args \
                     finished, the arguments were too large, or the arguments were emitted as an empty string. \
                     {retry_guidance} \
                     If you don't actually need to call this tool, skip it and continue.",
                    raw_excerpt,
                );
                return crate::tools::ToolOutput {
                    content: serde_json::json!({
                        "error": "missing_or_invalid_arguments",
                        "tool": name,
                        "message": msg,
                    })
                    .to_string(),
                    is_error: true,
                    meta: None,
                };
            }
        }

        if name == "publish_artifact" {
            return self.execute_publish_artifact(agent_id, input).await;
        }
        // Single-Agent Uplift Phase 1.2: todo_write 需要 DB（持久化到 agent_todos）+ emit
        // todo_update 事件让前端 TodoListPanel 实时刷新。所以特例化在 dispatch 层处理，
        // 与 publish_artifact 同模式，不污染 ToolExecutor 的 sandbox 边界。
        if name == crate::tools::TODO_WRITE_TOOL {
            return self.execute_todo_write(agent_id, input).await;
        }
        // Single-Agent Uplift Phase 2.4: enter_plan_mode 是纯"声明"工具——
        // 没有副作用，只把 plan 文本作为结构化事件 echo 给前端。直接在 dispatch
        // 层短路即可，不需要 ToolExecutor 沙箱、不需要 DB。
        if name == crate::tools::ENTER_PLAN_MODE_TOOL {
            return self.execute_enter_plan_mode(agent_id, input);
        }
        // Single-Agent Uplift B1: ask_user_question 阻塞等用户答 — 走专用路径。
        if name == crate::tools::ASK_USER_QUESTION_TOOL {
            return self.execute_ask_user_question(agent_id, input).await;
        }

        // FM-14 tool gate（write_file 到 protected_paths / shell_exec 到 destructive_commands）。
        if let Some(coord) = self
            .app_handle
            .try_state::<std::sync::Arc<crate::agent::approval::ApprovalCoordinator>>()
        {
            let db = self.app_handle.state::<Database>();
            let mission_id_opt: Option<String> = db
                .with_conn(|conn| queries::get_mission_id_for_agent(conn, agent_id))
                .ok()
                .flatten();
            if let Some(mission_id) = mission_id_opt {
                use crate::agent::approval_gate::{maybe_intercept_tool, ToolGateOutcome};
                match maybe_intercept_tool(
                    &self.app_handle,
                    coord.inner(),
                    db.inner(),
                    &self.cancel_token,
                    &mission_id,
                    agent_id,
                    name,
                    input,
                )
                .await
                {
                    ToolGateOutcome::Allow => {}
                    ToolGateOutcome::Rejected(out) => return out,
                }
            }
        }

        if let Some(handler) = self.custom_tool_handler.as_ref() {
            if handler.handles_tool(name) {
                return handler.execute_tool(name, input).await;
            }
        }

        // shell_exec 走带 stream 的入口，把 stdout/stderr emit 给前端 Workspace。
        // 其它工具透传到普通 execute，行为不变。
        self.tool_executor
            .execute_with_stream_context(
                name,
                input,
                &self.app_handle,
                agent_id,
                Some(crate::tools::ToolExecutionContext {
                    agent_id: agent_id.to_string(),
                    step,
                    tool_use_id: tool_use_id.to_string(),
                    tool_name: name.to_string(),
                }),
            )
            .await
    }

    /// Single-Agent Uplift Phase 1.2: 执行 todo_write 工具。
    ///
    /// 行为：
    ///   1. 解析 input.todos[]（{id, content, status}）；任何不合法格式都 is_error=true
    ///      让 LLM 重试（保持工具一致的"结构化错误"哲学）。
    ///   2. 调 queries::replace_agent_todos 全量替换 agent_todos 表。
    ///   3. emit `todo_update` 事件携带 todos 数组，前端 TodoListPanel 直接消费。
    ///   4. 工具返回值是简短文本（"Updated N todo(s)..."）让 LLM 不再啰嗦地把
    ///      整个清单复述一遍——前端看不到也无所谓，反正它只显示 panel。
    async fn execute_todo_write(
        &self,
        agent_id: &str,
        input: &serde_json::Value,
    ) -> crate::tools::ToolOutput {
        use crate::tools::ToolOutput;

        #[derive(serde::Deserialize)]
        struct TodoIn {
            id: String,
            content: String,
            status: String,
        }
        #[derive(serde::Deserialize)]
        struct TodoWriteInput {
            todos: Vec<TodoIn>,
        }

        let parsed: TodoWriteInput = match serde_json::from_value(input.clone()) {
            Ok(v) => v,
            Err(e) => {
                return ToolOutput {
                    content: serde_json::json!({
                        "error": "parameter_error",
                        "message": format!("todo_write input parse failed: {e}"),
                    })
                    .to_string(),
                    is_error: true,
                    meta: None,
                };
            }
        };

        // 校验 status 取值；invalid 直接拒绝并把允许的值告诉 LLM。
        const ALLOWED: &[&str] = &["pending", "in_progress", "completed", "cancelled"];
        for td in &parsed.todos {
            if !ALLOWED.contains(&td.status.as_str()) {
                return ToolOutput {
                    content: serde_json::json!({
                        "error": "parameter_error",
                        "message": format!(
                            "Invalid status `{}` for todo `{}`. Allowed: pending / in_progress / completed / cancelled.",
                            td.status, td.id
                        ),
                    })
                    .to_string(),
                    is_error: true,
                    meta: None,
                };
            }
        }
        // 校验"最多一个 in_progress"——不强制（语义建议，不写进契约），
        // 但触发时给 LLM 一个 hint 让它自己收敛。这条不阻塞写盘。
        let in_progress_count = parsed
            .todos
            .iter()
            .filter(|t| t.status == "in_progress")
            .count();

        let db = self.app_handle.state::<Database>();
        let agent_owned = agent_id.to_string();
        let inputs: Vec<(String, String, String)> = parsed
            .todos
            .iter()
            .map(|t| (t.id.clone(), t.content.clone(), t.status.clone()))
            .collect();

        let write_result = db.with_conn(move |conn| {
            let refs: Vec<crate::db::queries::TodoInput<'_>> = inputs
                .iter()
                .map(|(id, content, status)| crate::db::queries::TodoInput {
                    id: id.as_str(),
                    content: content.as_str(),
                    status: status.as_str(),
                })
                .collect();
            crate::db::queries::replace_agent_todos(conn, &agent_owned, &refs)
        });

        if let Err(e) = write_result {
            return ToolOutput {
                content: serde_json::json!({
                    "error": "db_error",
                    "message": format!("Failed to persist todos: {e}"),
                })
                .to_string(),
                is_error: true,
                meta: None,
            };
        }

        // emit todo_update 事件。meta 是完整 todo 列表（按数组顺序），前端按它整体刷新。
        let todos_meta: Vec<serde_json::Value> = parsed
            .todos
            .iter()
            .map(|t| {
                serde_json::json!({
                    "id": t.id,
                    "content": t.content,
                    "status": t.status,
                })
            })
            .collect();
        self.emit_event_with_meta(
            agent_id,
            self.read_agent_step(agent_id),
            "todo_update",
            &format!("Updated {} todo(s)", parsed.todos.len()),
            Some(serde_json::json!({ "todos": todos_meta })),
        );

        let mut summary = format!(
            "todos updated: {} item(s); {} pending, {} in_progress, {} completed",
            parsed.todos.len(),
            parsed
                .todos
                .iter()
                .filter(|t| t.status == "pending")
                .count(),
            in_progress_count,
            parsed
                .todos
                .iter()
                .filter(|t| t.status == "completed")
                .count(),
        );
        if in_progress_count > 1 {
            summary.push_str(
                " (note: prefer at most one in_progress at a time so progress is unambiguous)",
            );
        }
        ToolOutput {
            content: summary,
            is_error: false,
            meta: None,
        }
    }

    /// Single-Agent Uplift Phase 2.4: 执行 enter_plan_mode。
    ///
    /// 没有副作用：解析 input.plan 文本，emit 一条带结构化 meta 的 `system_hint`
    /// 让前端醒目展示（黄色边框 + 多行 plan），返回简短确认字符串给 LLM。
    /// 用 `system_hint` kind 是因为它已经接好了"突出展示"渲染（SystemHintLine），
    /// 且不需要单独建一个新 kind 进 migration。
    fn execute_enter_plan_mode(
        &self,
        agent_id: &str,
        input: &serde_json::Value,
    ) -> crate::tools::ToolOutput {
        use crate::tools::ToolOutput;
        let plan = match input.get("plan").and_then(|v| v.as_str()) {
            Some(p) if !p.trim().is_empty() => p.to_string(),
            _ => {
                return ToolOutput {
                    content: serde_json::json!({
                        "error": "parameter_error",
                        "message": "enter_plan_mode requires non-empty `plan` (markdown text).",
                    })
                    .to_string(),
                    is_error: true,
                    meta: None,
                };
            }
        };
        if plan_contains_tool_markup(&plan) {
            return ToolOutput {
                content: serde_json::json!({
                    "error": "plan_contains_tool_call_markup",
                    "message": "enter_plan_mode only records a plan; it does not execute tools. Remove embedded tool-call markup from the plan, then call the real write/read/shell tools in later steps. If the plan says to create required files early, the next tool call should actually write those files."
                })
                .to_string(),
                is_error: true,
                meta: Some(serde_json::json!({
                    "plan_contains_tool_call_markup": true,
                    "requires_actual_tool_call_next": true,
                })),
            };
        }
        let step = self.read_agent_step(agent_id);
        // 用 system_hint kind 复用前端 SystemHintLine 渲染。meta 里带 plan_mode flag
        // 让未来如果想拆出独立面板时可以前端查找。
        self.emit_event_with_meta(
            agent_id,
            step,
            "system_hint",
            &format!("[plan-mode] {}", plan.lines().next().unwrap_or("")),
            Some(serde_json::json!({
                "kind": "plan_mode",
                "plan": plan,
            })),
        );
        ToolOutput {
            content: format!(
                "Plan recorded ({} lines). Proceed with implementation; use todo_write to track step progress.",
                plan.lines().count()
            ),
            is_error: false,
            meta: None,
        }
    }

    /// Single-Agent Uplift B1: 执行 ask_user_question 工具。
    ///
    /// 行为：
    ///   1. 解析 input.questions[]；任何 schema 偏差立即结构化报错，让 LLM 自己重试。
    ///   2. 用 uuid 生成 session_id，注册到 user_questions 全局表拿到 oneshot::Receiver。
    ///   3. emit 一条 `system_hint` 事件，meta.kind="ask_user_question"，meta.session_id
    ///      + meta.questions 让前端 AskUserQuestionLine 渲染卡片。
    ///   4. select! 等：
    ///        - oneshot 完成 → 用户答了 → 返回 JSON answers 给 LLM
    ///        - 30 分钟超时 → 撤销 session，返回 timed_out=true
    ///        - cancel_token 触发 → 撤销 session，返回 cancelled
    ///   5. 不论哪条分支结束都 emit 一条 `system_hint` (kind=`"ask_user_question_resolved"`)
    ///      让前端从"等输入"UI 退回到"已回答"摘要。
    async fn execute_ask_user_question(
        &self,
        agent_id: &str,
        input: &serde_json::Value,
    ) -> crate::tools::ToolOutput {
        use crate::tools::ToolOutput;

        enum AskOutcome {
            Answered(crate::agent::user_questions::UserAnswerSet),
            TimedOut,
            Cancelled,
        }

        #[derive(serde::Deserialize)]
        struct OptionIn {
            id: String,
            label: String,
        }
        #[derive(serde::Deserialize)]
        struct QuestionIn {
            id: String,
            prompt: String,
            options: Vec<OptionIn>,
            #[serde(default)]
            allow_multiple: bool,
        }
        #[derive(serde::Deserialize)]
        struct AskInput {
            questions: Vec<QuestionIn>,
        }

        let parsed: AskInput = match serde_json::from_value(input.clone()) {
            Ok(v) => v,
            Err(e) => {
                return ToolOutput {
                    content: serde_json::json!({
                        "error": "parameter_error",
                        "message": format!("ask_user_question input parse failed: {e}"),
                    })
                    .to_string(),
                    is_error: true,
                    meta: None,
                };
            }
        };
        if parsed.questions.is_empty() {
            return ToolOutput {
                content: serde_json::json!({
                    "error": "parameter_error",
                    "message": "ask_user_question requires at least one question.",
                })
                .to_string(),
                is_error: true,
                meta: None,
            };
        }
        for q in &parsed.questions {
            if q.options.len() < 2 {
                return ToolOutput {
                    content: serde_json::json!({
                        "error": "parameter_error",
                        "message": format!(
                            "Question `{}` has fewer than 2 options. Provide multiple choices \
                             so the user has something to pick.", q.id
                        ),
                    })
                    .to_string(),
                    is_error: true,
                    meta: None,
                };
            }
        }

        let session_id = uuid::Uuid::new_v4().to_string();
        let rx = crate::agent::user_questions::register(&session_id);

        let questions_meta = serde_json::json!(parsed
            .questions
            .iter()
            .map(|q| serde_json::json!({
                "id": q.id,
                "prompt": q.prompt,
                "allow_multiple": q.allow_multiple,
                "options": q.options.iter().map(|o| serde_json::json!({
                    "id": o.id,
                    "label": o.label,
                })).collect::<Vec<_>>(),
            }))
            .collect::<Vec<_>>());

        let step = self.read_agent_step(agent_id);
        self.emit_event_with_meta(
            agent_id,
            step,
            "system_hint",
            &format!(
                "[ask-user] {} question(s) — waiting for your answer",
                parsed.questions.len()
            ),
            Some(serde_json::json!({
                "kind": "ask_user_question",
                "session_id": session_id,
                "agent_id": agent_id,
                "questions": questions_meta,
            })),
        );

        // 30 分钟兜底；和工具描述里的 timeout 一致。
        let timeout = std::time::Duration::from_secs(30 * 60);

        let outcome = tokio::select! {
            biased;
            _ = self.cancel_token.cancelled() => {
                crate::agent::user_questions::drop_pending(&session_id);
                AskOutcome::Cancelled
            }
            res = rx => {
                match res {
                    Ok(answers) => AskOutcome::Answered(answers),
                    Err(_) => AskOutcome::Cancelled, // sender dropped
                }
            }
            _ = tokio::time::sleep(timeout) => {
                crate::agent::user_questions::drop_pending(&session_id);
                AskOutcome::TimedOut
            }
        };

        // 通知前端：不管哪条分支都让卡片切回"已结束"。
        let resolution_meta = match &outcome {
            AskOutcome::Answered(set) => serde_json::json!({
                "kind": "ask_user_question_resolved",
                "session_id": session_id,
                "outcome": "answered",
                "answers": set.answers,
            }),
            AskOutcome::TimedOut => serde_json::json!({
                "kind": "ask_user_question_resolved",
                "session_id": session_id,
                "outcome": "timed_out",
            }),
            AskOutcome::Cancelled => serde_json::json!({
                "kind": "ask_user_question_resolved",
                "session_id": session_id,
                "outcome": "cancelled",
            }),
        };
        let summary_text = match &outcome {
            AskOutcome::Answered(_) => "[ask-user] answers received",
            AskOutcome::TimedOut => "[ask-user] timed out (no answer in 30 min)",
            AskOutcome::Cancelled => "[ask-user] cancelled",
        };
        self.emit_event_with_meta(
            agent_id,
            step,
            "system_hint",
            summary_text,
            Some(resolution_meta),
        );

        // 给 LLM 的返回值：保持简单 JSON。answered 时把 question id → option id 列表 echo 回去。
        let payload = match outcome {
            AskOutcome::Answered(set) => serde_json::json!({
                "session_id": session_id,
                "answers": set.answers,
            }),
            AskOutcome::TimedOut => serde_json::json!({
                "session_id": session_id,
                "timed_out": true,
                "hint": "User did not answer in 30 minutes. Pick a sensible default and proceed, \
                        or call task_complete to report you cannot continue.",
            }),
            AskOutcome::Cancelled => serde_json::json!({
                "session_id": session_id,
                "cancelled": true,
            }),
        };
        ToolOutput {
            content: payload.to_string(),
            is_error: false,
            meta: None,
        }
    }

    /// Helper：从 DB 读 agent.current_step，给那些没有 step 上下文的代码路径用
    /// （比如 todo_write 不在主循环 step 里被调用——其实在的，但这层封装让加点更随意）。
    fn read_agent_step(&self, agent_id: &str) -> u32 {
        let db = self.app_handle.state::<Database>();
        db.with_conn(|conn| {
            conn.query_row(
                "SELECT current_step FROM agents WHERE id = ?1",
                rusqlite::params![agent_id],
                |row| row.get::<_, i64>(0),
            )
            .map(|v| v as u32)
            .or_else(|_| Ok(0u32))
        })
        .unwrap_or(0)
    }

    /// 执行 publish_artifact 工具：基于 agent_id 反查 task_id / mission_id，
    /// 调用 artifacts 模块的校验 + 持久化路径。
    async fn execute_publish_artifact(
        &self,
        agent_id: &str,
        input: &serde_json::Value,
    ) -> crate::tools::ToolOutput {
        use crate::agent::artifacts::{record_publish, PublishArtifactInput};
        use crate::tools::ToolOutput;
        let parsed: PublishArtifactInput = match serde_json::from_value(input.clone()) {
            Ok(v) => v,
            Err(e) => {
                return ToolOutput {
                    content: serde_json::json!({
                        "error": "parameter_error",
                        "message": format!("publish_artifact input parse failed: {e}"),
                    })
                    .to_string(),
                    is_error: true,
                    meta: None,
                };
            }
        };
        let db = self.app_handle.state::<Database>();
        let agent = agent_id.to_string();
        let workspace = self.workspace_root.clone();
        let parsed_for_db = parsed.clone();
        let result = db.with_conn(move |conn| {
            let Some(task_id) = queries::get_task_id_for_agent(conn, &agent)? else {
                return Ok(None);
            };
            let mission_id = queries::get_mission_id_for_agent(conn, &agent)?
                .ok_or_else(|| anyhow::anyhow!("agent {agent} has no mission binding"))?;
            let decls_json: String = conn
                .query_row(
                    "SELECT produces_artifacts FROM tasks WHERE id = ?1",
                    rusqlite::params![&task_id],
                    |row| row.get::<_, String>(0),
                )
                .unwrap_or_else(|_| "[]".to_string());
            let decls: Vec<crate::agent::artifacts::ArtifactDecl> =
                serde_json::from_str(&decls_json).unwrap_or_default();
            record_publish(
                conn,
                &workspace,
                &mission_id,
                &task_id,
                &parsed_for_db,
                Some(&decls),
            )
            .map(Some)
            .map_err(|e| anyhow::anyhow!(e.to_string()))
        });
        match result {
            Ok(Some(artifact)) => {
                let _ = self.app_handle.emit(
                    "artifact-published",
                    serde_json::json!({
                        "agentId": agent_id,
                        "artifactId": artifact.id,
                        "missionId": artifact.mission_id,
                        "taskId": artifact.producer_task_id,
                        "type": artifact.artifact_type,
                        "localName": artifact.local_name,
                        "filePaths": artifact.file_paths,
                    }),
                );
                ToolOutput {
                    content: format!(
                        "Published artifact `{}` ({}) with {} file(s).",
                        artifact.local_name,
                        artifact.artifact_type,
                        artifact.file_paths.len()
                    ),
                    is_error: false,
                    meta: None,
                }
            }
            Ok(None) => ToolOutput {
                content: format!(
                    "Artifact `{}` accepted for this dev-only agent with {} file(s).",
                    parsed.local_name,
                    parsed.file_paths.len()
                ),
                is_error: false,
                meta: None,
            },
            Err(e) => ToolOutput {
                content: serde_json::json!({
                    "error": "artifact_error",
                    "message": e.to_string(),
                })
                .to_string(),
                is_error: true,
                meta: None,
            },
        }
    }

    /// 执行 guardrails 并决定后续动作。
    ///
    /// `task_description` 与 `summary` 一并传给 `LlmJudge`，作为评判的素材。
    async fn evaluate_completion(
        &self,
        agent_id: &str,
        step: u32,
        summary: &str,
        opts: &AgentRunOptions,
    ) -> CompletionOutcome {
        let db = self.app_handle.state::<Database>();
        let (task_id_opt, mission_id_opt) = match db.with_conn(|conn| {
            let task_id = queries::get_task_id_for_agent(conn, agent_id)?;
            let mission_id = queries::get_mission_id_for_agent(conn, agent_id)?;
            Ok((task_id, mission_id))
        }) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!("evaluate_completion: cannot resolve agent->task: {e}");
                return CompletionOutcome::Completed;
            }
        };

        let has_task = task_id_opt.is_some();
        let has_explicit_contract = opts
            .task_contract
            .as_ref()
            .map(|contract| !contract.is_empty())
            .unwrap_or(false);
        let has_explicit_guardrails =
            !opts.guardrails.is_empty() || !opts.produces.is_empty() || has_explicit_contract;
        let task_id = match task_id_opt {
            Some(t) => t,
            None if has_explicit_guardrails => {
                tracing::warn!(
                    "Agent {agent_id} has no task; running explicit completion guardrails with synthetic task id"
                );
                agent_id.to_string()
            }
            None => {
                tracing::warn!("Agent {agent_id} has no task; treating task_complete as success");
                return CompletionOutcome::Completed;
            }
        };
        let mission_id = mission_id_opt.unwrap_or_default();

        // 取 LLM provider（LlmJudge 用）。失败时退化为 None（LlmJudge 走 warn+pass 路径）。
        let (llm_for_judge, model_for_judge): (
            Option<std::sync::Arc<dyn crate::llm::LlmProvider>>,
            Option<String>,
        ) = match crate::commands::build_provider(&self.app_handle) {
            Ok((p, m)) => (Some(p), Some(m)),
            Err(_) => (None, None),
        };

        // 取 task description（LlmJudge 上下文）
        let task_desc_for_judge: Option<String> = if has_task {
            db.with_conn(|conn| {
                conn.query_row(
                    "SELECT description FROM tasks WHERE id = ?1",
                    rusqlite::params![&task_id],
                    |row| row.get::<_, String>(0),
                )
                .map(Some)
                .or_else(|_| Ok(None))
            })
            .unwrap_or(None)
        } else {
            None
        };

        let ctx = GuardrailContext {
            task_id: &task_id,
            mission_id: &mission_id,
            repo_root: &self.workspace_root,
            expected_output: opts.expected_output.clone(),
            produces: opts.produces.clone(),
            task_description: task_desc_for_judge,
            completion_summary: Some(summary.to_string()),
            llm: llm_for_judge,
            default_model: model_for_judge,
        };

        let guardrails: Vec<Guardrail> = if opts.guardrails.is_empty() {
            // 即便 task 没显式声明 guardrail，仍跑一次"产出对账" 以避免 Agent 谎称完成
            if !opts.produces.is_empty() {
                vec![Guardrail::ArtifactsExist]
            } else {
                Vec::new()
            }
        } else {
            opts.guardrails.clone()
        };

        if let Some(contract) = opts
            .task_contract
            .as_ref()
            .filter(|contract| !contract.is_empty())
        {
            let contract_ctx = ContractContext {
                repo_root: &self.workspace_root,
                completion_summary: Some(summary),
            };
            let result = task_contract::validate_task_contract(contract, &contract_ctx);
            let serialized = serde_json::to_string(&result.reports).unwrap_or_default();
            let reports_meta = serde_json::to_value(&result.reports).ok();
            self.emit_event_with_meta(
                agent_id,
                step,
                if result.all_passed {
                    "contract_pass"
                } else {
                    "contract_fail"
                },
                &serialized,
                reports_meta,
            );
            if !result.all_passed {
                return CompletionOutcome::Retry {
                    feedback: result.format_failure_for_agent(),
                };
            }
        }

        if guardrails.is_empty() {
            self.emit_event(
                agent_id,
                step,
                "guardrail_summary",
                "no legacy guardrails configured after task contract validation; accepting task_complete",
            );
            return CompletionOutcome::Completed;
        }

        let result = guardrail::run_guardrails(&guardrails, &ctx, &db).await;
        let serialized = serde_json::to_string(&result.reports).unwrap_or_default();
        // Single-Agent Uplift Phase 0.2: 把 reports 直接作为 meta，前端可按 array 渲染
        // 每条 guardrail 的 pass/fail badge + 折叠 detail，不再让用户看一坨 JSON 串。
        let reports_meta = serde_json::to_value(&result.reports).ok();
        self.emit_event_with_meta(
            agent_id,
            step,
            if result.all_passed {
                "guardrail_pass"
            } else {
                "guardrail_fail"
            },
            &serialized,
            reports_meta,
        );
        let _ = summary; // 仅用于事件层；持久化在 caller 处
        if result.all_passed {
            CompletionOutcome::Completed
        } else {
            CompletionOutcome::Retry {
                feedback: result.format_failure_for_agent(),
            }
        }
    }

    fn finish_cancelled(&self, agent_id: &str, step: u32) -> Result<AgentStatus> {
        self.expire_agent_notes(agent_id);
        self.emit_event(agent_id, step, "status_change", "cancelled");
        self.update_agent_status(agent_id, "cancelled");
        self.mark_task_failed_with_reason(agent_id, "cancelled", "cancelled: user stop");
        Ok(AgentStatus::Cancelled)
    }

    /// 在 engine 层把失败原因写入 `tasks.last_error` + `agents.error_message`，让前端 DAG /
    /// TaskDetailPanel 能直接 hover 看为什么红了。`reason` 推荐带分类前缀
    /// （`timeout:` / `max_steps:` / `guardrail:` / `cancelled:` / `llm_error:`）。
    async fn precheck_guardrails_for_repair(
        &self,
        agent_id: &str,
        step: u32,
        opts: &AgentRunOptions,
        required_output_files: &[String],
        messages: &mut Vec<Message>,
    ) -> bool {
        let summary = format!(
            "Late guardrail precheck before finalization. Required output artifact(s): {}.",
            required_output_files.join(", ")
        );
        match self
            .evaluate_completion(agent_id, step, &summary, opts)
            .await
        {
            CompletionOutcome::Completed => {
                self.emit_event_with_meta(
                    agent_id,
                    step,
                    "system_hint",
                    "Late guardrail precheck passed; continue finalizing and call task_complete.",
                    Some(serde_json::json!({
                        "kind": "guardrail_precheck_pass",
                        "required_output_files": required_output_files,
                    })),
                );
                false
            }
            CompletionOutcome::Retry { feedback } => {
                let feedback = feedback.replace(
                    "You called task_complete, but the following guardrail checks did NOT pass.",
                    "The current required artifacts do not yet pass the completion guardrails.",
                );
                let repair_feedback =
                    guardrail_repair_instruction(&feedback, required_output_files);
                let hint = format!(
                    "[System] Late guardrail precheck failed while there is still time to repair.\n\n{repair_feedback}"
                );
                messages.push(Message {
                    role: MessageRole::User,
                    content: vec![ContentBlock::Text { text: hint.clone() }],
                    cache_control: None,
                });
                self.emit_event_with_meta(
                    agent_id,
                    step,
                    "system_hint",
                    &hint,
                    Some(serde_json::json!({
                        "kind": "guardrail_precheck_retry",
                        "required_output_files": required_output_files,
                    })),
                );
                true
            }
        }
    }

    async fn try_auto_complete_on_step_exhaustion(
        &self,
        agent_id: &str,
        step: u32,
        opts: &AgentRunOptions,
        required_output_files: &[String],
        messages: &mut Vec<Message>,
    ) -> Result<Option<AgentStatus>> {
        self.try_auto_complete_required_outputs_ready(
            agent_id,
            step,
            opts,
            required_output_files,
            messages,
            "max_steps_auto_finalize",
            "Reached the step limit after creating the required output artifact(s). Auto-finalizing because required artifacts are present and guardrails will decide completion.",
        )
        .await
    }

    async fn try_auto_complete_required_outputs_ready(
        &self,
        agent_id: &str,
        step: u32,
        opts: &AgentRunOptions,
        required_output_files: &[String],
        messages: &mut Vec<Message>,
        event_kind: &str,
        summary_prefix: &str,
    ) -> Result<Option<AgentStatus>> {
        if required_output_files.is_empty() {
            return Ok(None);
        }
        let (_, missing_required_files) =
            required_files_status(&self.workspace_root, required_output_files);
        if !missing_required_files.is_empty() {
            return Ok(None);
        }

        let summary = format!(
            "{summary_prefix} Required output artifact(s): {}.",
            required_output_files.join(", ")
        );
        self.emit_event_with_meta(
            agent_id,
            step,
            "system_hint",
            &summary,
            Some(serde_json::json!({
                "kind": event_kind,
                "required_output_files": required_output_files,
            })),
        );

        let hook_ctx = self.build_hook_context(
            agent_id,
            step,
            crate::agent::hooks::HookPhase::Stop,
            messages,
            None,
            Some(summary.clone()),
        );
        match self
            .dispatch_hook_phase(agent_id, step, hook_ctx, messages)
            .await
        {
            Ok(()) => {}
            Err(HookFatal::Terminal(reason)) => {
                let msg = format!("Stop hook terminated auto-finalization: {reason}");
                self.mark_task_failed_with_reason(agent_id, "failed", &msg);
                self.emit_event(agent_id, step, "status_change", "failed");
                self.update_agent_status(agent_id, "failed");
                return Ok(Some(AgentStatus::Failed));
            }
            Err(HookFatal::StepAborted(_)) => return Ok(None),
        }

        match self
            .evaluate_completion(agent_id, step, &summary, opts)
            .await
        {
            CompletionOutcome::Completed => {
                self.emit_event(agent_id, step, "message", &summary);
                self.persist_completion_summary(agent_id, &summary);
                let hook_ctx = self.build_hook_context(
                    agent_id,
                    step,
                    crate::agent::hooks::HookPhase::TaskCompleted,
                    messages,
                    None,
                    Some(summary.clone()),
                );
                match self
                    .dispatch_hook_phase(agent_id, step, hook_ctx, messages)
                    .await
                {
                    Ok(()) => {}
                    Err(HookFatal::Terminal(reason)) => {
                        let msg = format!(
                            "TaskCompleted hook terminated auto-finalization after summary persisted: {reason}"
                        );
                        self.mark_task_failed_with_reason(agent_id, "failed", &msg);
                        self.emit_event(agent_id, step, "status_change", "failed");
                        self.update_agent_status(agent_id, "failed");
                        return Ok(Some(AgentStatus::Failed));
                    }
                    Err(HookFatal::StepAborted(_)) => {
                        let msg = "TaskCompleted hook requested non-terminal step abort during auto-finalization; treating as failed to avoid completed-but-not-finalized state".to_string();
                        self.mark_task_failed_with_reason(agent_id, "failed", &msg);
                        self.emit_event(agent_id, step, "status_change", "failed");
                        self.update_agent_status(agent_id, "failed");
                        return Ok(Some(AgentStatus::Failed));
                    }
                }
                self.emit_event(agent_id, step, "status_change", "completed");
                self.update_agent_status(agent_id, "completed");
                self.expire_agent_notes(agent_id);
                Ok(Some(AgentStatus::Completed))
            }
            CompletionOutcome::Retry { feedback } => {
                let repair_feedback =
                    guardrail_repair_instruction(&feedback, required_output_files);
                self.emit_event_with_meta(
                    agent_id,
                    step,
                    "guardrail_fail",
                    &repair_feedback,
                    Some(serde_json::json!({
                        "kind": "max_steps_auto_finalize_guardrail_retry",
                    })),
                );
                messages.push(Message {
                    role: MessageRole::User,
                    content: vec![ContentBlock::Text {
                        text: repair_feedback,
                    }],
                    cache_control: None,
                });
                Ok(Some(AgentStatus::Running))
            }
        }
    }

    fn mark_task_failed_with_reason(&self, agent_id: &str, status: &str, reason: &str) {
        let db = self.app_handle.state::<Database>();
        let aid = agent_id.to_string();
        let st = status.to_string();
        let r = reason.to_string();
        let _ = db.with_conn(move |conn| {
            queries::fail_task_for_agent(conn, &aid, &st, &r)?;
            conn.execute(
                "UPDATE agents SET error_message = COALESCE(error_message, ?2), \
                 updated_at = datetime('now') WHERE id = ?1",
                rusqlite::params![&aid, &r],
            )?;
            Ok(())
        });
    }

    fn emit_event(&self, agent_id: &str, step: u32, kind: &str, content: &str) {
        self.emit_event_with_meta(agent_id, step, kind, content, None);
    }

    /// Single-Agent Uplift Phase 0.2: emit + persist 一个带结构化 meta 的事件。
    /// `meta` 不为 None 时同时进 `agent-event` 推送和 `agent_events.meta` 列。
    /// 前端按 kind 决定如何解析（不同 kind 的 schema 各不一样，记得保持兼容）。
    fn emit_event_with_meta(
        &self,
        agent_id: &str,
        step: u32,
        kind: &str,
        content: &str,
        meta: Option<serde_json::Value>,
    ) {
        let _ = self.app_handle.emit(
            "agent-event",
            AgentEventPayload {
                agent_id: agent_id.to_string(),
                step,
                kind: kind.to_string(),
                content: content.to_string(),
                meta: meta.clone(),
            },
        );

        self.persist_event(agent_id, step, kind, content, meta);
    }

    fn persist_event(
        &self,
        agent_id: &str,
        step: u32,
        kind: &str,
        content: &str,
        meta: Option<serde_json::Value>,
    ) {
        let db = self.app_handle.state::<Database>();
        let event_id = Uuid::new_v4().to_string();
        let meta_str = meta.as_ref().map(|v| v.to_string());

        if let Err(e) = db.with_conn(|conn| {
            crate::db::queries::insert_event_with_meta(
                conn,
                &event_id,
                agent_id,
                step as i64,
                kind,
                content,
                meta_str.as_deref(),
            )
        }) {
            tracing::warn!("Failed to persist agent event (kind={kind}): {e}");
        }
    }

    fn update_agent_status(&self, agent_id: &str, status: &str) {
        let db = self.app_handle.state::<Database>();
        if let Err(e) = db.with_conn(|conn| {
            conn.execute(
                "UPDATE agents SET status = ?1, updated_at = datetime('now') WHERE id = ?2",
                rusqlite::params![status, agent_id],
            )?;
            Ok(())
        }) {
            tracing::warn!("Failed to update agent status: {e}");
        }
    }

    /// P2-1 Phase B：在指定 phase 执行注册的所有 hook，处理结果（emit 事件 + 注入 messages）。
    ///
    /// 返回值语义：
    /// - `Ok(())`：继续主循环（Pass 或 Inject 后已 push 到 messages）
    /// - `Err(HookFatal::Terminal(reason))`：terminal=true 的 Prevent，调用方应立即把
    ///   agent 标 failed
    /// - `Err(HookFatal::StepAborted(reason))`：terminal=false 的 Prevent，调用方应
    ///   提前结束当前 step（不再调 LLM / 不再执行 tool）但 agent 仍存活
    ///
    /// 不变量：
    /// - empty registry → 无任何 emit，立即 Pass 返回（**0 行为变化**，向后兼容）
    /// - 多 hook InjectMessage → 按注册顺序拼接成单条 user message 注入，避免
    ///   message 列表被 hook 数撑爆
    /// - hook execute panic 已被 `tokio::task` 默认 catch（async fn）；为防御
    ///   "hook execute 阻塞 N 分钟"主循环僵死，外层 caller 可选择性套 timeout
    ///   （Phase C 引入 CommandHook 时再加，本地内置 hook 假设 short-running）
    async fn dispatch_hook_phase(
        &self,
        agent_id: &str,
        step: u32,
        ctx: crate::agent::hooks::HookContext,
        messages: &mut Vec<Message>,
    ) -> std::result::Result<(), HookFatal> {
        // 快速路径：空 registry 不做任何 IO / event emit，避免在主循环热路径上
        // 把每个 step × 7 phase = 多个 event 写日志噪音化。
        if self.hook_registry.hook_count() == 0 {
            return Ok(());
        }
        let phase = ctx.phase;
        let started = std::time::Instant::now();
        let result = self.hook_registry.execute_phase(&ctx).await;
        let duration_ms = started.elapsed().as_millis() as u64;
        match result {
            crate::agent::hooks::PhaseResult::Pass => {
                // 仅在 registry 非空时记一笔 trace 事件，便于排查"hook 配置上来了但没生效"
                let meta = serde_json::json!({
                    "phase": phase.as_str(),
                    "duration_ms": duration_ms,
                    "hook_count": self.hook_registry.hook_count(),
                });
                self.emit_event_with_meta(
                    agent_id,
                    step,
                    "hook_executed",
                    &format!("hooks pass ({})", phase.as_str()),
                    Some(meta),
                );
                Ok(())
            }
            crate::agent::hooks::PhaseResult::Injected(injs) => {
                let total = injs.len();
                // 单条聚合消息：每个 hook 一段，前缀 hook_name + severity
                let body = injs
                    .iter()
                    .map(|i| {
                        format!(
                            "[hook:{} severity={}] {}",
                            i.hook_name,
                            i.severity.as_str(),
                            i.content
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n\n");
                let injected_user_msg = format!(
                    "Hooks injected feedback in phase `{}`. Address these before continuing:\n\n{body}",
                    phase.as_str()
                );
                messages.push(Message {
                    role: MessageRole::User,
                    content: vec![ContentBlock::Text {
                        text: injected_user_msg,
                    }],
                    cache_control: None,
                });
                let meta = serde_json::json!({
                    "phase": phase.as_str(),
                    "duration_ms": duration_ms,
                    "injections": injs
                        .iter()
                        .map(|i| serde_json::json!({
                            "hook_name": i.hook_name,
                            "severity": i.severity.as_str(),
                        }))
                        .collect::<Vec<_>>(),
                });
                self.emit_event_with_meta(
                    agent_id,
                    step,
                    "hook_inject",
                    &format!("{total} hook injection(s) in {}", phase.as_str()),
                    Some(meta),
                );
                Ok(())
            }
            crate::agent::hooks::PhaseResult::Prevented {
                hook_name,
                reason,
                terminal,
            } => {
                let meta = serde_json::json!({
                    "phase": phase.as_str(),
                    "duration_ms": duration_ms,
                    "hook_name": hook_name,
                    "reason": reason,
                    "terminal": terminal,
                });
                self.emit_event_with_meta(
                    agent_id,
                    step,
                    "hook_prevented",
                    &format!(
                        "hook `{hook_name}` prevented continuation in {} (terminal={terminal}): {reason}",
                        phase.as_str()
                    ),
                    Some(meta),
                );
                tracing::warn!(
                    agent_id = %agent_id,
                    step,
                    phase = phase.as_str(),
                    hook_name = %hook_name,
                    terminal,
                    "hook prevented continuation: {reason}"
                );
                if terminal {
                    Err(HookFatal::Terminal(reason))
                } else {
                    Err(HookFatal::StepAborted(reason))
                }
            }
        }
    }

    /// 构造给 hook 用的 [`HookContext`]。
    ///
    /// 把 engine 内部状态裁剪成 hook 可见的 owned 副本——见 hooks/mod.rs 的设计取舍。
    /// `last_assistant_text` cap 1KB / `recent_tool_uses` cap 5 项防止上下文撑爆。
    fn build_hook_context(
        &self,
        agent_id: &str,
        step: u32,
        phase: crate::agent::hooks::HookPhase,
        messages: &[Message],
        last_tool_use: Option<crate::agent::hooks::HookToolUseInfo>,
        task_complete_summary: Option<String>,
    ) -> crate::agent::hooks::HookContext {
        let total_count = messages.len();
        let last_assistant_text = messages
            .iter()
            .rev()
            .find(|m| matches!(m.role, MessageRole::Assistant))
            .and_then(|m| {
                m.content.iter().find_map(|b| match b {
                    ContentBlock::Text { text } => {
                        let s: String = text.chars().take(1024).collect();
                        Some(s)
                    }
                    _ => None,
                })
            });
        // recent_tool_uses: 倒序收集，cap 5 项
        let mut recent_tool_uses: Vec<String> = Vec::with_capacity(5);
        for m in messages.iter().rev() {
            for b in &m.content {
                if let ContentBlock::ToolUse { name, .. } = b {
                    if recent_tool_uses.len() < 5 {
                        recent_tool_uses.push(name.clone());
                    }
                }
            }
            if recent_tool_uses.len() >= 5 {
                break;
            }
        }
        // mission_id 反查；失败时塞空串（hook 大多用不到）
        // mission_id 反查：try_state→ok→with_conn 链式可能为 None / Err / 0 rows，
        // 任一失败都 fallback 到空串（hook 大多用不到）。
        let mission_id = self
            .app_handle
            .try_state::<Database>()
            .and_then(|db| {
                let task_id = db
                    .with_conn(|conn| queries::get_task_id_for_agent(conn, agent_id))
                    .ok()
                    .flatten()?;
                db.with_conn(|conn| {
                    conn.query_row(
                        "SELECT mission_id FROM tasks WHERE id = ?1",
                        rusqlite::params![&task_id],
                        |r| r.get::<_, String>(0),
                    )
                    .map_err(anyhow::Error::from)
                })
                .ok()
            })
            .unwrap_or_default();
        crate::agent::hooks::HookContext {
            agent_id: agent_id.to_string(),
            mission_id,
            workspace_path: self.workspace_root.to_string_lossy().into_owned(),
            step,
            phase,
            messages_summary: crate::agent::hooks::HookMessagesSummary {
                total_count,
                last_assistant_text,
                recent_tool_uses,
            },
            last_tool_use,
            task_complete_summary,
        }
    }

    /// P1-2 Phase B：持久化 cross-model fallback 累计切换次数到 agents 表。
    ///
    /// 失败仅 warn 不 fail——这是观测指标，落库失败不应影响 agent 主循环。
    /// 调用方在 fallback 实际发生时立即调用一次（典型 mission 全程 0~3 次）。
    fn persist_fallback_switches(&self, agent_id: &str, total: u32) {
        let db = self.app_handle.state::<Database>();
        if let Err(e) = db.with_conn(|conn| {
            conn.execute(
                "UPDATE agents SET fallback_switches_total = ?1, \
                 updated_at = datetime('now') WHERE id = ?2",
                rusqlite::params![total as i64, agent_id],
            )?;
            Ok(())
        }) {
            tracing::warn!(
                agent_id = %agent_id,
                fallback_switches_total = total,
                "Failed to persist fallback_switches_total: {e}"
            );
        }
    }

    fn persist_completion_summary(&self, agent_id: &str, summary: &str) {
        if summary.trim().is_empty() {
            return;
        }
        let db = self.app_handle.state::<Database>();
        let agent = agent_id.to_string();
        let summary_owned = summary.to_string();
        if let Err(e) = db.with_conn(move |conn| {
            if let Some(task_id) = queries::get_task_id_for_agent(conn, &agent)? {
                conn.execute(
                    "UPDATE tasks SET completion_summary = ?1 WHERE id = ?2",
                    rusqlite::params![summary_owned, task_id],
                )?;
            }
            Ok(())
        }) {
            tracing::warn!("Failed to persist completion summary: {e}");
        }
    }

    fn persist_task_handoff_packet(&self, agent_id: &str, task_complete_input: &serde_json::Value) {
        let db = self.app_handle.state::<Database>();
        let agent = agent_id.to_string();
        let input = task_complete_input.clone();
        if let Err(e) = db.with_conn(move |conn| {
            use rusqlite::OptionalExtension;

            let Some(task_id) = queries::get_task_id_for_agent(conn, &agent)? else {
                return Ok(());
            };

            let Some((mission_id, title, description)) = conn
                .query_row(
                    "SELECT mission_id, title, COALESCE(description, '') FROM tasks WHERE id = ?1",
                    rusqlite::params![&task_id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                        ))
                    },
                )
                .optional()?
            else {
                tracing::warn!(agent_id = %agent, task_id = %task_id, "Cannot persist handoff: task row not found");
                return Ok(());
            };

            let generation_status = if task_complete_handoff_is_agent_authored(&input) {
                "agent_authored"
            } else {
                "fallback"
            };

            if generation_status == "fallback" {
                if let Some(existing) = queries::get_task_handoff_packet(conn, &task_id)? {
                    if existing.generation_status == "agent_authored" {
                        tracing::debug!(
                            task_id = %task_id,
                            "Skipping fallback handoff persistence because an agent-authored packet already exists"
                        );
                        return Ok(());
                    }
                }
            }

            let published_artifacts = queries::list_artifacts_for_task(conn, &task_id)?
                .into_iter()
                .filter(|artifact| artifact.published)
                .map(|artifact| {
                    let file_paths = serde_json::from_str::<Vec<String>>(&artifact.file_paths)
                        .unwrap_or_default();
                    DeliveryArtifactRef {
                        artifact_id: Some(artifact.id),
                        local_name: artifact.local_name,
                        artifact_type: artifact.artifact_type,
                        summary: artifact.summary,
                        file_paths,
                    }
                })
                .collect::<Vec<_>>();

            let packet = TaskHandoffPacket::from_task_complete_value(
                &mission_id,
                &task_id,
                title,
                description,
                &input,
                published_artifacts,
            )?;
            let packet_json = serde_json::to_string(&packet)?;
            queries::upsert_task_handoff_packet(
                conn,
                &task_id,
                &mission_id,
                &packet_json,
                generation_status,
            )?;
            Ok(())
        }) {
            tracing::warn!(agent_id = %agent_id, "Failed to persist task handoff packet: {e}");
        }
    }

    fn persist_cost_record(
        &self,
        agent_id: &str,
        model: &str,
        input_tokens: u64,
        output_tokens: u64,
        cost_usd: f64,
    ) {
        let db = self.app_handle.state::<Database>();
        let record_id = Uuid::new_v4().to_string();
        if let Err(e) = db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO cost_records (id, agent_id, model, input_tokens, output_tokens, cost_usd)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![record_id, agent_id, model, input_tokens as i64, output_tokens as i64, cost_usd],
            )?;
            Ok(())
        }) {
            tracing::warn!("Failed to persist cost record: {e}");
        }
    }

    fn accumulate_agent_cost(
        &self,
        agent_id: &str,
        input_tokens: u64,
        output_tokens: u64,
        cost_usd: f64,
    ) {
        let db = self.app_handle.state::<Database>();
        let total_tokens = input_tokens + output_tokens;
        if let Err(e) = db.with_conn(|conn| {
            conn.execute(
                "UPDATE agents SET tokens_used = tokens_used + ?1, cost_usd = cost_usd + ?2, updated_at = datetime('now') WHERE id = ?3",
                rusqlite::params![total_tokens as i64, cost_usd, agent_id],
            )?;
            Ok(())
        }) {
            tracing::warn!("Failed to accumulate agent cost: {e}");
        }
    }

    fn update_agent_step(&self, agent_id: &str, step: u32) {
        let db = self.app_handle.state::<Database>();
        if let Err(e) = db.with_conn(|conn| {
            conn.execute(
                "UPDATE agents SET current_step = ?1, updated_at = datetime('now') WHERE id = ?2",
                rusqlite::params![step, agent_id],
            )?;
            Ok(())
        }) {
            tracing::warn!("Failed to update agent step: {e}");
        }
    }

    fn describe_llm_call(step: u32, messages: &[Message]) -> String {
        if messages.is_empty() {
            return format!("Step {step}: Analyzing task and planning approach");
        }
        let last_assistant = messages
            .iter()
            .rev()
            .find(|m| m.role == MessageRole::Assistant);
        if let Some(assistant_msg) = last_assistant {
            let tool_names: Vec<&str> = assistant_msg
                .content
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::ToolUse { name, .. } => Some(name.as_str()),
                    _ => None,
                })
                .collect();
            if !tool_names.is_empty() {
                let last_user = messages.last();
                let has_errors = last_user
                    .map(|m| {
                        m.content
                            .iter()
                            .any(|b| matches!(b, ContentBlock::ToolResult { is_error: true, .. }))
                    })
                    .unwrap_or(false);
                let tools_str = tool_names.join(", ");
                return if has_errors {
                    format!("Step {step}: Reviewing results (with errors) from {tools_str}")
                } else {
                    format!("Step {step}: Reviewing results from {tools_str}")
                };
            }
        }
        format!("Step {step}: Continuing analysis")
    }

    // ---- FM-06: Note helpers ----

    fn poll_queued_notes(&self, agent_id: &str) -> Vec<(String, String)> {
        let db = self.app_handle.state::<Database>();
        db.with_conn(|conn| {
            let notes = queries::poll_queued_notes(conn, agent_id)?;
            Ok(notes.into_iter().map(|n| (n.id, n.content)).collect())
        })
        .unwrap_or_default()
    }

    fn mark_notes_applied(&self, note_ids: &[String]) {
        if note_ids.is_empty() {
            return;
        }
        let db = self.app_handle.state::<Database>();
        if let Err(e) = db.with_conn(|conn| queries::mark_notes_applied(conn, note_ids)) {
            tracing::warn!("Failed to mark notes as applied: {e}");
        }
    }

    fn expire_agent_notes(&self, agent_id: &str) {
        let db = self.app_handle.state::<Database>();
        match db.with_conn(|conn| queries::expire_notes_for_agent(conn, agent_id)) {
            Ok(count) if count > 0 => {
                tracing::info!("Expired {count} queued note(s) for agent {agent_id}");
            }
            Err(e) => {
                tracing::warn!("Failed to expire notes for agent {agent_id}: {e}");
            }
            _ => {}
        }
    }

    fn format_notes_for_injection(notes: &[(String, String)]) -> String {
        let mut out = String::from(
            "[System Note - Priority Update from Commander]:\n\
             The following directive(s) have been issued by the human commander. \
             You MUST follow them and adjust your work accordingly, \
             even if it means modifying files you have already written.\n\n",
        );
        for (i, (_, content)) in notes.iter().enumerate() {
            if notes.len() > 1 {
                out.push_str(&format!("{}. {content}\n\n", i + 1));
            } else {
                out.push_str(&format!("{content}\n\n"));
            }
        }
        out.push_str("Please take this into account in your next steps.");
        out
    }
}

enum CompletionOutcome {
    Completed,
    Retry { feedback: String },
}

fn render_task_contract_brief(contract: Option<&TaskContract>) -> String {
    let Some(contract) = contract.filter(|contract| !contract.is_empty()) else {
        return String::new();
    };
    let mut lines = Vec::new();
    lines.push("\n- Active task contract validation is enabled; task_complete will be accepted only after the declared final response and artifact contract passes.".to_string());
    if let Some(final_response) = &contract.final_response {
        if final_response.required {
            let mut parts = vec!["final response required".to_string()];
            match final_response.format {
                task_contract::FinalResponseFormat::Json => parts.push("valid JSON".to_string()),
                task_contract::FinalResponseFormat::Text => parts.push("text".to_string()),
                task_contract::FinalResponseFormat::Any => {}
            }
            if final_response.fenced {
                parts.push("fenced code block".to_string());
            }
            if !final_response.required_json_keys.is_empty() {
                parts.push(format!(
                    "keys: {}",
                    final_response.required_json_keys.join(", ")
                ));
            }
            lines.push(format!(
                "  - Final response contract: {}.",
                parts.join("; ")
            ));
        }
    }
    if !contract.artifacts.is_empty() {
        lines.push(format!(
            "  - Required artifact contract(s): {}. Create these artifacts early, keep them non-empty and parseable, replace placeholders, then validate before task_complete.",
            contract.required_artifact_paths().join(", ")
        ));
        let text_length_constraints = contract
            .artifacts
            .iter()
            .filter_map(|artifact| {
                let mut parts = Vec::new();
                if let Some(min) = artifact.min_text_chars {
                    parts.push(format!("at least {min} chars"));
                }
                if let Some(max) = artifact.max_non_ws_chars {
                    parts.push(format!("at most {max} non-whitespace chars"));
                }
                (!parts.is_empty()).then(|| format!("{} ({})", artifact.path, parts.join(", ")))
            })
            .collect::<Vec<_>>();
        if !text_length_constraints.is_empty() {
            lines.push(format!(
                "  - Text length contract(s): {}. Keep generated text artifacts within these bounds before task_complete.",
                text_length_constraints.join("; ")
            ));
        }
        let notebook_paths = contract
            .artifacts
            .iter()
            .filter(|artifact| {
                artifact.kind == task_contract::ArtifactKind::Notebook
                    || artifact.path.ends_with(".ipynb")
            })
            .map(|artifact| artifact.path.as_str())
            .collect::<Vec<_>>();
        if !notebook_paths.is_empty() {
            let static_paths = contract
                .artifacts
                .iter()
                .filter(|artifact| artifact.require_static_visible_derived_values)
                .map(|artifact| artifact.path.as_str())
                .collect::<Vec<_>>();
            if !static_paths.is_empty() {
                lines.push(format!(
                    "  - Notebook static visibility contract: for {}, if a code cell computes final values from variables, keep the derivation and also make the final computed constants visibly present in notebook source comments, assertions, literals, or saved outputs.",
                    static_paths.join(", ")
                ));
            }
        }
    }
    if contract.source_grounding.is_some() {
        lines.push("  - Source grounding contract: preserve required quoted/source marker substrings verbatim in the final response when requested.".to_string());
    }
    lines.join("\n")
}

fn render_guardrail_brief(guardrails: &[Guardrail]) -> String {
    if guardrails.is_empty() {
        return String::new();
    }
    let mut out = String::from("\n- Active guardrails for completion: ");
    let names: Vec<&str> = guardrails.iter().map(|g| g.name()).collect();
    out.push_str(&names.join(", "));
    out
}

fn render_produces_brief(produces: &[(String, String)]) -> String {
    if produces.is_empty() {
        return String::new();
    }
    let lines: Vec<String> = produces
        .iter()
        .map(|(name, ty)| format!("  - {name} ({ty})"))
        .collect();
    format!("\n\n## Required Artifacts\n{}", lines.join("\n"))
}

#[cfg(test)]
mod transient_network_retry_tests {
    use super::{llm_error_looks_transient_network, transient_network_retry_delay};

    #[test]
    fn detects_transient_request_errors() {
        assert!(llm_error_looks_transient_network(
            "error sending request for url (https://api.example/v1/chat/completions)"
        ));
        assert!(llm_error_looks_transient_network(
            "connection reset by peer"
        ));
        assert!(llm_error_looks_transient_network(
            "dns error: failed to lookup address"
        ));
        assert!(!llm_error_looks_transient_network(
            "context length exceeded"
        ));
        assert!(!llm_error_looks_transient_network("invalid API key"));
    }

    #[test]
    fn transient_retry_delay_is_bounded_exponential() {
        assert_eq!(transient_network_retry_delay(1, 500).as_millis(), 500);
        assert_eq!(transient_network_retry_delay(2, 500).as_millis(), 1000);
        assert_eq!(transient_network_retry_delay(99, 500).as_millis(), 16000);
        assert_eq!(transient_network_retry_delay(1, 0).as_millis(), 250);
    }
}
#[cfg(test)]
mod char_safe_excerpt_tests {
    use super::char_safe_excerpt;

    #[test]
    fn truncates_multibyte_text_without_panicking() {
        let text = r##"{"content":"# DAPO 论文分享 PPT — 结构说明与信息来源\n\n这是中文内容"}"##;
        let excerpt = char_safe_excerpt(text, 40);
        assert!(excerpt.contains("DAPO"));
        assert!(excerpt.contains("chars"));
    }

    #[test]
    fn leaves_short_text_unchanged() {
        assert_eq!(char_safe_excerpt("hello 中文", 20), "hello 中文");
    }
}

#[cfg(test)]
mod idle_retry_budget_tests {
    //! 回归测试：`next_idle_retry_budget` 是 idle-retry 设计的契约函数。
    //!
    //! 任何"简化"——例如永远 reset / 永远不 reset / reset 条件反了——都会让
    //! 用户经历两类回归：
    //!  - 永远 reset → 等价于无限 retry，遇到真挂的 provider task 会跑到 max_steps 才挂
    //!  - 永远不 reset → 回到任务级 budget 的老 bug，长 task 撞 3 次卡就 failed
    //!
    //! 守住这一组小不变量就能让未来的重构不至于摔进同一个坑。
    use super::next_idle_retry_budget;

    #[test]
    fn new_step_resets_to_default() {
        assert_eq!(next_idle_retry_budget(false, 0, 2), 2);
        assert_eq!(next_idle_retry_budget(false, 1, 2), 2);
        assert_eq!(next_idle_retry_budget(false, 2, 2), 2);
    }

    #[test]
    fn retry_continuation_keeps_current() {
        // 第一次 retry 后剩 1
        assert_eq!(next_idle_retry_budget(true, 1, 2), 1);
        // 第二次连续 retry 后剩 0
        assert_eq!(next_idle_retry_budget(true, 0, 2), 0);
    }

    #[test]
    fn full_step_lifecycle_two_retries_then_recover() {
        // 模拟一个 step：默认 budget=2，连续 2 次 retry，然后 step 成功 → 下一个 step 重置回 2
        let default = 2u32;

        // 进入新 step
        let mut budget = next_idle_retry_budget(false, 99, default);
        assert_eq!(budget, 2, "新 step 必须重置");

        // 第一次 IdleTimeout
        budget -= 1;
        budget = next_idle_retry_budget(true, budget, default);
        assert_eq!(budget, 1, "retry 续命，不重置");

        // 第二次 IdleTimeout
        budget -= 1;
        budget = next_idle_retry_budget(true, budget, default);
        assert_eq!(budget, 0, "再次 retry 续命，依然不重置");

        // 这一步 LLM 终于回了完整 response，进入下一个 step（resume = false）
        budget = next_idle_retry_budget(false, budget, default);
        assert_eq!(budget, 2, "step 成功后下一个 step 必须再次重置回满");
    }

    #[test]
    fn zero_default_disables_retry() {
        // 用户/未来配置如果把 budget 设为 0，整套机制等价于"卡就 fail"——
        // 这是合法配置，函数不应抛错或返回 surprising 值。
        assert_eq!(next_idle_retry_budget(false, 0, 0), 0);
        assert_eq!(next_idle_retry_budget(true, 0, 0), 0);
    }
}

#[cfg(test)]
mod context_compaction_tests {
    //! Single-Agent Uplift Phase 2.2 回归测试。
    //! 这两个不变量丢了用户立刻会感知到（要么 prompt 爆 ctx，要么 LLM 突然失忆）：
    //!   ① apply_tool_result_budget 幂等 + 只截 >8KB 的 ToolResult
    //!   ② microcompact 至少 8 messages 才动手；动手后总 token 一定下降
    use super::*;

    fn user_msg(text: &str) -> Message {
        Message {
            role: MessageRole::User,
            content: vec![ContentBlock::Text {
                text: text.to_string(),
            }],
            cache_control: None,
        }
    }

    fn assistant_with_tool_use(name: &str, tool_use_id: &str) -> Message {
        Message {
            role: MessageRole::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: tool_use_id.to_string(),
                name: name.to_string(),
                input: serde_json::json!({}),
            }],
            cache_control: None,
        }
    }

    fn user_with_tool_result(tool_use_id: &str, content: &str) -> Message {
        Message {
            role: MessageRole::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: tool_use_id.to_string(),
                content: content.to_string(),
                is_error: false,
            }],
            cache_control: None,
        }
    }

    /// 短 ToolResult（< 8KB）不应被截断。
    #[test]
    fn budget_keeps_small_results_intact() {
        let mut messages = vec![user_with_tool_result("t1", "small content")];
        apply_tool_result_budget(&mut messages);
        if let ContentBlock::ToolResult { content, .. } = &messages[0].content[0] {
            assert_eq!(content, "small content");
        } else {
            panic!("expected ToolResult");
        }
    }

    /// 大 ToolResult 会被替换成 sentinel 字串，包含尾部内容。
    #[test]
    fn budget_truncates_large_results_with_tail() {
        let big = "X".repeat(20_000); // ~20K chars > 8K budget
        let mut messages = vec![user_with_tool_result("t1", &big)];
        apply_tool_result_budget(&mut messages);
        if let ContentBlock::ToolResult { content, .. } = &messages[0].content[0] {
            assert!(content.starts_with(TRUNCATED_SENTINEL_PREFIX));
            assert!(content.contains("Original size:"));
            assert!(content.len() < big.len() / 2, "截断后必须显著变短");
        } else {
            panic!("expected ToolResult");
        }
    }

    /// 截断幂等：连跑两次结果一致（不会把 sentinel 自己当原内容再截）。
    #[test]
    fn budget_is_idempotent() {
        let big = "X".repeat(20_000);
        let mut messages = vec![user_with_tool_result("t1", &big)];
        apply_tool_result_budget(&mut messages);
        let first = if let ContentBlock::ToolResult { content, .. } = &messages[0].content[0] {
            content.clone()
        } else {
            unreachable!()
        };
        apply_tool_result_budget(&mut messages);
        let second = if let ContentBlock::ToolResult { content, .. } = &messages[0].content[0] {
            content.clone()
        } else {
            unreachable!()
        };
        assert_eq!(first, second, "幂等：第二次应保持不变");
    }

    #[test]
    fn compacts_only_old_completed_tool_use_inputs() {
        let huge = "print('x')\n".repeat(600);
        let recent = "print('recent')\n".repeat(600);
        let mut messages = vec![
            Message {
                role: MessageRole::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: "old_done".to_string(),
                    name: "write_file".to_string(),
                    input: serde_json::json!({"path":"big.py","content": huge}),
                }],
                cache_control: None,
            },
            user_with_tool_result("old_done", "ok"),
        ];
        for i in 0..TOOL_USE_INPUT_RECENT_MESSAGE_WINDOW {
            messages.push(user_msg(&format!("recent context {i}")));
        }
        messages.push(Message {
            role: MessageRole::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "recent_done".to_string(),
                name: "write_file".to_string(),
                input: serde_json::json!({"path":"recent.py","content": recent}),
            }],
            cache_control: None,
        });
        messages.push(user_with_tool_result("recent_done", "ok"));
        messages.push(Message {
            role: MessageRole::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "pending".to_string(),
                name: "write_file".to_string(),
                input: serde_json::json!({"path":"pending.py","content": "x".repeat(5000)}),
            }],
            cache_control: None,
        });

        let compacted = compact_large_tool_use_inputs(&mut messages);
        assert_eq!(compacted, 1);
        let ContentBlock::ToolUse { input, .. } = &messages[0].content[0] else {
            panic!("expected tool use");
        };
        assert_eq!(input["__tool_use_input_compacted__"], true);
        assert_eq!(input["__non_executable_history_stub__"], true);
        assert_eq!(input["original_path"], "big.py");
        assert!(input.get("path").is_none());
        assert!(input.get("excerpt").is_none());
        assert!(input.get("command_excerpt").is_none());
        assert!(input.get("__tool_use_input_excerpt__").is_some());
        assert!(serde_json::to_string(input).unwrap().len() < 1400);
        let ContentBlock::ToolUse { input, .. } = &messages[messages.len() - 3].content[0] else {
            panic!("expected recent tool use");
        };
        assert!(input.get("__tool_use_input_compacted__").is_none());
        assert!(input.get("content").is_some());
        let ContentBlock::ToolUse { input, .. } = &messages[messages.len() - 1].content[0] else {
            panic!("expected pending tool use");
        };
        assert!(input.get("__tool_use_input_compacted__").is_none());
        assert!(input.get("content").is_some());
    }

    /// messages 太少时 microcompact 不动手——避免短任务上下文丢失。
    #[test]
    fn microcompact_noop_for_short_history() {
        let mut messages: Vec<Message> = (0..5).map(|i| user_msg(&format!("m{i}"))).collect();
        let report = microcompact(&mut messages);
        assert!(report.is_none(), "<8 messages 不该动手");
        assert_eq!(messages.len(), 5);
    }

    /// 长历史压缩：丢 1/3，最前面插 summary，token 总数下降。
    #[test]
    fn microcompact_drops_oldest_third_and_inserts_summary() {
        let mut messages: Vec<Message> = Vec::new();
        // 12 条 user/assistant 交替，每条带些"内容"凑出非零 token 估算
        for i in 0..6 {
            messages.push(user_msg(&format!("user msg {i} ").repeat(20)));
            messages.push(assistant_with_tool_use("read_file", &format!("tu_{i}")));
        }
        let before_count = messages.len();
        let before_tokens = approximate_tokens(&messages);

        let report = microcompact(&mut messages).expect("应当压缩");
        assert!(report.dropped_messages >= 1);
        // 摘要插到最前
        assert!(matches!(messages[0].role, MessageRole::User));
        if let ContentBlock::Text { text } = &messages[0].content[0] {
            assert!(text.starts_with("[context-compact]"));
            assert!(text.contains("read_file"), "summary 应汇总用过的工具名");
        } else {
            panic!("summary 必须是 Text block");
        }
        // 总数 = 原数 - drop + 1（summary）
        assert_eq!(messages.len(), before_count - report.dropped_messages + 1);
        // tokens 下降（用近似估算）
        let after_tokens = approximate_tokens(&messages);
        assert!(after_tokens < before_tokens, "压缩后 tokens 必须下降");
    }

    #[test]
    fn microcompact_does_not_orphan_tool_result() {
        let mut messages: Vec<Message> = vec![
            user_msg(&"warmup ".repeat(20)),
            assistant_with_tool_use("read_file", "tu_boundary"),
            user_with_tool_result("tu_boundary", &"result ".repeat(20)),
        ];
        for i in 0..5 {
            messages.push(user_msg(&format!("later user msg {i} ").repeat(20)));
        }

        let report =
            microcompact(&mut messages).expect("should compact with pairing-safe boundary");
        assert_eq!(report.dropped_messages, 3);
        assert!(messages.iter().all(|msg| {
            !msg.content.iter().any(|block| {
                matches!(
                    block,
                    ContentBlock::ToolResult { tool_use_id, .. } if tool_use_id == "tu_boundary"
                )
            })
        }));
    }

    #[test]
    fn reactive_compact_does_not_orphan_tool_result() {
        let mut messages: Vec<Message> = vec![
            user_msg(&"warmup ".repeat(20)),
            assistant_with_tool_use("read_file", "tu_boundary"),
            user_with_tool_result("tu_boundary", &"result ".repeat(20)),
            user_msg(&"later ".repeat(20)),
        ];

        let report = reactive_compact_aggressive(&mut messages)
            .expect("should compact with pairing-safe boundary");
        assert_eq!(report.dropped_messages, 1);
        assert!(messages.iter().any(|msg| {
            msg.content.iter().any(|block| {
                matches!(
                    block,
                    ContentBlock::ToolUse { id, .. } if id == "tu_boundary"
                )
            })
        }));
        assert!(messages.iter().any(|msg| {
            msg.content.iter().any(|block| {
                matches!(
                    block,
                    ContentBlock::ToolResult { tool_use_id, .. } if tool_use_id == "tu_boundary"
                )
            })
        }));
    }

    // ---- Single-Agent Uplift P0-1: reactive compact ----
    //
    // 守住 reactive compact 的核心不变量：
    //   ① messages < 4 时返回 None（让 caller bail，不死循环）
    //   ② messages >= 4 时压一半 + 显式 "reactive" 摘要标签
    //   ③ 反复调用（模拟死循环 retry）仍按规则压，不 panic 不溢出
    //   ④ 比 proactive microcompact 更激进

    /// messages < 4 时 reactive_compact_aggressive 不动手，返回 None。
    /// 这是 caller 区分"还能救一次" vs "真的没办法"的关键信号。
    #[test]
    fn reactive_compact_returns_none_when_too_few_messages() {
        let mut messages: Vec<Message> =
            vec![user_msg("first"), user_msg("second"), user_msg("third")];
        let report = reactive_compact_aggressive(&mut messages);
        assert!(
            report.is_none(),
            "<4 messages 必须返回 None 让 caller 真 bail"
        );
        assert_eq!(messages.len(), 3, "返回 None 时 messages 不应被破坏");
    }

    /// reactive 摘要必须明确标注 "reactive" 来源——前端依此区分 proactive 主动压缩
    /// 和 "API 拒绝后" 被动压缩，UX 上加红色徽章警示。
    #[test]
    fn reactive_compact_summary_marks_reactive_origin() {
        // 8 条消息，包含 2 个工具调用 (前半 read_file, 后半 edit_file)
        // 让我们能验证 tools_seen 只汇总被 drop 的前半部分。
        let mut messages: Vec<Message> = Vec::new();
        for i in 0..4 {
            messages.push(user_msg(&format!("user msg {i} ").repeat(10)));
            messages.push(assistant_with_tool_use(
                if i < 2 { "read_file" } else { "edit_file" },
                &format!("tu_{i}"),
            ));
        }
        let before_count = messages.len();

        let report = reactive_compact_aggressive(&mut messages).expect("8 条消息应能压");
        // 8 / 2 = 4 条被 drop
        assert_eq!(report.dropped_messages, 4);
        assert!(
            report.tools_seen.contains(&"read_file".to_string()),
            "drop 的前半含 read_file 应被汇总"
        );

        // 摘要插到最前 + 包含 "reactive" 显式标签
        assert!(matches!(messages[0].role, MessageRole::User));
        if let ContentBlock::Text { text } = &messages[0].content[0] {
            assert!(
                text.contains("[context-compact:reactive]"),
                "摘要必须显式标注 reactive 来源，前端按此渲染红色徽章"
            );
            assert!(
                text.contains("API rejected"),
                "摘要必须告诉 LLM 触发原因，让其下一轮更谨慎"
            );
        } else {
            panic!("reactive 摘要必须是 Text block");
        }
        // 总数 = 原数 - drop + 1（summary）
        assert_eq!(messages.len(), before_count - report.dropped_messages + 1);
    }

    #[test]
    fn working_memory_compact_triggers_before_microcompact_and_keeps_refs() {
        let mut messages: Vec<Message> = Vec::new();
        messages.push(user_msg(
            "Create report.md and inspect .miragenty/evidence/a/step-0001/t/stdout.txt",
        ));
        for i in 0..14 {
            messages.push(assistant_with_tool_use("read_file", &format!("wm_tu_{i}")));
            let content = if i == 0 {
                "     1|Meeting notes:\n     2|The deployment report captured 7 service checks in the release checklist.\n     3|The smoke-test workflow can finish in 1-2 steps through scripted validation."
                    .to_string()
            } else {
                format!(
                    "read src/file_{i}.rs and report_{i}.md with useful facts {}",
                    "x".repeat(200)
                )
            };
            messages.push(user_with_tool_result(&format!("wm_tu_{i}"), &content));
        }
        assert!(approximate_tokens(&messages) < MICROCOMPACT_TOKEN_THRESHOLD);
        assert!(should_working_memory_compact(&messages));
        let report =
            working_memory_compact(&mut messages).expect("working memory compact should run");
        assert!(report.dropped_messages > 0);
        if let ContentBlock::Text { text } = &messages[0].content[0] {
            assert!(text.starts_with("[working-memory]"));
            assert!(text.contains("read_file"));
            assert!(text.contains(".miragenty/evidence/a/step-0001/t/stdout.txt"));
            assert!(text.contains("report.md"));
            assert!(text.contains("Key compacted observations"));
            assert!(text.contains("7 service checks"));
            assert!(text.contains("1-2 steps through scripted validation"));
        } else {
            panic!("working memory summary must be text");
        }
    }

    #[test]
    fn guardrail_precheck_starts_before_final_tool_window() {
        assert!(GUARDRAIL_PRECHECK_REMAINING_STEPS > STEPS_REMAINING_HINT);
        assert!(ARTIFACT_CHECKPOINT_REMAINING_STEPS > STEPS_REMAINING_HINT);
    }

    #[test]
    fn long_task_policy_blocks_planning_only_near_end() {
        assert!(long_task_policy_allows_tool(
            "todo_write",
            LONG_TASK_FINALIZATION_STEPS + 1
        ));
        assert!(!long_task_policy_allows_tool(
            "todo_write",
            LONG_TASK_FINALIZATION_STEPS
        ));
        assert!(!long_task_policy_allows_tool("enter_plan_mode", 1));
        assert!(long_task_policy_allows_tool("write_file", 1));
        assert!(long_task_policy_allows_tool("task_complete", 0));
        let feedback = long_task_policy_feedback("todo_write", 3);
        assert!(feedback.contains("finalization phase"));
        assert!(feedback.contains("task_complete"));
    }

    #[test]
    fn read_only_loop_blocks_only_more_reading() {
        assert!(!read_only_loop_allows_tool(
            "read_file",
            &serde_json::json!({})
        ));
        assert!(!read_only_loop_allows_tool("grep", &serde_json::json!({})));
        assert!(!read_only_loop_allows_tool("glob", &serde_json::json!({})));
        assert!(!read_only_loop_allows_tool(
            "list_files",
            &serde_json::json!({})
        ));
        assert!(!read_only_loop_allows_tool(
            "shell_exec",
            &serde_json::json!({"command": "find . -name requirements.txt | head -20"})
        ));
        assert!(!read_only_loop_allows_tool(
            "shell_exec",
            &serde_json::json!({"command": "curl -s https://example.com | head -20"})
        ));
        assert!(tool_is_read_only_loop_exploration(
            "shell_exec",
            &serde_json::json!({"command": "curl -s https://example.com | head -20"})
        ));
        assert!(read_only_loop_allows_tool(
            "write_file",
            &serde_json::json!({})
        ));
        assert!(read_only_loop_allows_tool(
            "shell_exec",
            &serde_json::json!({"command": "python3 create_ppt.py"})
        ));
        assert!(!read_only_loop_allows_tool(
            "shell_exec",
            &serde_json::json!({"command": "curl https://example.com/paper"})
        ));
        assert!(read_only_loop_allows_tool(
            "shell_exec",
            &serde_json::json!({"command": "curl -L https://example.com/figure.png -o figure.png"})
        ));
        assert!(read_only_loop_allows_tool(
            "shell_exec",
            &serde_json::json!({"command": "wget https://example.com/data.csv -O data.csv"})
        ));
        let feedback = read_only_loop_block_feedback(
            "read_file",
            &["presentation.pptx".into(), "presentation_notes.md".into()],
        );
        assert!(feedback.contains("read/search loop"));
        assert!(feedback.contains("presentation.pptx"));
    }

    #[test]
    fn direct_output_loop_feedback_requires_task_complete() {
        assert!(has_direct_response_guardrail(&[
            Guardrail::SummaryMatches {
                mode: crate::agent::guardrail::SummaryMatchMode::JsonCodeBlock,
            },
            Guardrail::SummaryJsonValid {
                require_non_empty: true,
            },
        ]));
        assert!(!has_direct_response_guardrail(&[
            Guardrail::FilesNonEmpty {
                globs: vec!["report.md".into()],
            }
        ]));
        let feedback = direct_output_loop_block_feedback("grep");
        assert!(feedback.contains("direct-response task"));
        assert!(feedback.contains("task_complete.summary"));
        assert!(feedback.contains("Do not write files"));
    }

    #[test]
    fn required_file_outputs_collects_file_guardrails() {
        let opts = AgentRunOptions {
            guardrails: vec![
                Guardrail::FilesNonEmpty {
                    globs: vec!["report.json".into()],
                },
                Guardrail::FilesJsonValid {
                    globs: vec!["report.json".into(), "notes/*.json".into()],
                    require_non_empty: true,
                },
                Guardrail::SummaryNonEmpty,
            ],
            ..AgentRunOptions::default()
        };
        assert_eq!(
            required_file_outputs(&opts),
            vec!["notes/*.json".to_string(), "report.json".to_string()]
        );
    }

    #[test]
    fn required_files_status_tracks_missing_empty_and_present() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("present.json"), "{\"ok\":true}").unwrap();
        std::fs::write(tmp.path().join("empty.json"), "").unwrap();
        let globs = vec![
            "present.json".to_string(),
            "empty.json".to_string(),
            "missing.json".to_string(),
        ];
        let (present, missing) = required_files_status(tmp.path(), &globs);
        assert_eq!(present, vec!["present.json".to_string()]);
        assert_eq!(
            missing,
            vec!["empty.json".to_string(), "missing.json".to_string()]
        );
    }

    #[test]
    fn required_files_status_treats_placeholder_json_as_missing() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("skeleton.json"),
            r#"{
              "base_model": "",
              "hardware_env": "",
              "critical_libs": [],
              "is_feasible_on_8xa100_80gb": false,
              "reason": ""
            }"#,
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("ready.json"),
            r#"{
              "base_model": "Qwen2.5-32B",
              "hardware_env": "8xA100 80GB",
              "critical_libs": ["verl", "vllm", "torch"],
              "is_feasible_on_8xa100_80gb": true,
              "reason": "Model and optimizer state fit with tensor parallelism."
            }"#,
        )
        .unwrap();

        let globs = vec!["skeleton.json".to_string(), "ready.json".to_string()];
        let (present, missing) = required_files_status(tmp.path(), &globs);
        assert_eq!(present, vec!["ready.json".to_string()]);
        assert_eq!(missing, vec!["skeleton.json".to_string()]);
    }

    #[test]
    fn artifact_checkpoint_allows_delivery_and_artifact_shell_only() {
        for name in [
            "write_file",
            "edit_file",
            "notebook_edit",
            "publish_artifact",
            "task_complete",
        ] {
            assert!(artifact_checkpoint_allows_tool(
                name,
                &serde_json::json!({})
            ));
        }

        assert!(artifact_checkpoint_allows_tool(
            "shell_exec",
            &serde_json::json!({"command": "python3 create_ppt.py && test -s presentation.pptx"})
        ));
        assert!(artifact_checkpoint_allows_tool(
            "shell_exec",
            &serde_json::json!({"command": "node render.js > report.json"})
        ));
        assert!(artifact_checkpoint_allows_tool(
            "shell_exec",
            &serde_json::json!({"command": "python3 create_ppt.py"})
        ));
        assert!(artifact_checkpoint_allows_tool(
            "shell_exec",
            &serde_json::json!({"command": "chmod +x create_pptx.py && ./create_pptx.py"})
        ));
        assert!(artifact_checkpoint_allows_tool(
            "shell_exec",
            &serde_json::json!({"command": "python3 - <<'PY'\nfrom pptx import Presentation\nprs = Presentation()\nprs.save('presentation.pptx')\nPY"})
        ));
        assert!(artifact_checkpoint_allows_tool(
            "shell_exec",
            &serde_json::json!({"command": "cat > build_pptx.py <<'PY'\nfrom pptx import Presentation\nprs = Presentation()\nprs.save('presentation.pptx')\nPY"})
        ));
        assert!(artifact_checkpoint_allows_tool(
            "shell_exec",
            &serde_json::json!({"command": "python3 - <<'PY'\nfrom pathlib import Path\nPath('generate_artifacts.py').write_text('print(1)')\nPY"})
        ));

        for name in [
            "read_file",
            "grep",
            "search_files",
            "glob",
            "list_files",
            "todo_write",
            "enter_plan_mode",
        ] {
            assert!(!artifact_checkpoint_allows_tool(
                name,
                &serde_json::json!({})
            ));
        }
        assert!(!artifact_checkpoint_allows_tool(
            "shell_exec",
            &serde_json::json!({"command": "curl https://example.com/paper"})
        ));
        assert!(!artifact_checkpoint_allows_tool(
            "shell_exec",
            &serde_json::json!({"command": "python3 -c \"lines=open('create_pptx.py').readlines(); print(lines[160])\""})
        ));
        assert!(!artifact_checkpoint_allows_tool(
            "shell_exec",
            &serde_json::json!({"command": "grep -n Method paper.txt"})
        ));
    }

    #[test]
    fn urgent_artifact_checkpoint_focuses_on_missing_artifacts() {
        let missing = vec![
            "presentation.pptx".to_string(),
            "presentation_notes.md".to_string(),
        ];
        assert!(artifact_checkpoint_allows_tool_for_remaining_steps(
            "write_file",
            &serde_json::json!({"path":"presentation_notes.md","content":"notes"}),
            &missing,
            1,
        ));
        assert!(artifact_checkpoint_allows_tool_for_remaining_steps(
            "write_file",
            &serde_json::json!({"path":"build_pptx.py","content":"script"}),
            &missing,
            1,
        ));
        assert!(artifact_checkpoint_allows_tool_for_remaining_steps(
            "shell_exec",
            &serde_json::json!({"command":"python3 - <<'PY'\nfrom pathlib import Path\nPath('presentation.pptx').write_bytes(b'pptx')\nPath('presentation_notes.md').write_text('notes')\nPY"}),
            &missing,
            1,
        ));
        assert!(!artifact_checkpoint_allows_tool_for_remaining_steps(
            "read_file",
            &serde_json::json!({"path":"presentation.pptx"}),
            &missing,
            1,
        ));
        assert!(artifact_checkpoint_allows_tool_for_remaining_steps(
            "shell_exec",
            &serde_json::json!({"command":"python3 inspect_ppt.py presentation.pptx"}),
            &missing,
            1,
        ));
        assert!(artifact_checkpoint_allows_tool_for_remaining_steps(
            "shell_exec",
            &serde_json::json!({"command":"python3 -c \"open('presentation.pptx','wb').write(b'x')\""}),
            &missing,
            1,
        ));
        assert!(!artifact_checkpoint_allows_tool_for_remaining_steps(
            "shell_exec",
            &serde_json::json!({"command":"python3 inspect_ppt.py presentation.pptx"}),
            &missing,
            4,
        ));
    }
    #[test]
    fn artifact_checkpoint_feedback_names_missing_files() {
        let feedback = artifact_checkpoint_feedback(
            "read_file",
            &["presentation.pptx".into(), "presentation_notes.md".into()],
            12,
        );
        assert!(feedback.contains("read_file"));
        assert!(feedback.contains("12 step"));
        assert!(feedback.contains("presentation.pptx"));
        assert!(feedback.contains("presentation_notes.md"));
        assert!(feedback.contains("Create or repair") || feedback.contains("Create the missing"));
        assert!(feedback.contains("do not patch or inspect helper scripts"));
    }
    #[test]
    fn timeout_finalization_allows_only_delivery_tools() {
        let empty = serde_json::json!({});
        assert!(finalization_allows_tool("write_file", &empty));
        assert!(finalization_allows_tool("edit_file", &empty));
        assert!(finalization_allows_tool("notebook_edit", &empty));
        assert!(finalization_allows_tool("publish_artifact", &empty));
        assert!(finalization_allows_tool("task_complete", &empty));
        assert!(!finalization_allows_tool("read_file", &empty));
        assert!(!finalization_allows_tool("grep", &empty));
        assert!(!finalization_allows_tool(
            "shell_exec",
            &serde_json::json!({"command": "python3 -c \"from PIL import Image; print('PIL available')\""})
        ));
        assert!(finalization_allows_tool(
            "shell_exec",
            &serde_json::json!({"command": "python3 create_ppt.py && test -s presentation.pptx"})
        ));
    }

    #[test]
    fn malformed_write_file_recovery_escalates_and_resets_after_delivery() {
        let malformed_input = serde_json::json!({
            crate::llm::ARG_PARSE_ERROR_KEY: "invalid JSON: EOF while parsing a string",
            crate::llm::ARG_RAW_KEY: r#"{"path":"gen.py","content":"unterminated"#,
        });
        assert!(tool_input_has_arg_parse_error(&malformed_input));

        let malformed_block = (
            "tu_bad".to_string(),
            "write_file".to_string(),
            malformed_input,
        );
        let malformed_output = Some(crate::tools::ToolOutput {
            content: serde_json::json!({"error":"missing_or_invalid_arguments"}).to_string(),
            is_error: true,
            meta: None,
        });
        let mut state = InvalidToolArgsRecoveryState::default();
        let first = state
            .observe_tool_batch(
                &[malformed_block.clone()],
                &[malformed_output.clone()],
                &["presentation.pptx".into()],
            )
            .expect("first malformed write should hint");
        assert!(first.contains("Do not retry a large write_file payload"));
        assert_eq!(state.malformed_write_file_count, 1);

        let second = state
            .observe_tool_batch(
                &[malformed_block.clone()],
                &[malformed_output.clone()],
                &["presentation.pptx".into()],
            )
            .expect("second malformed write should escalate");
        assert!(second.contains("Recovery Escalation"));
        assert!(second.contains("malformed 2 times"));
        assert!(second.contains("shell command or heredoc"));

        let partial_block = (
            "tu_partial".to_string(),
            "write_file".to_string(),
            serde_json::json!({"path":"gen_ppt.py","append":true,"content":"partial"}),
        );
        let partial_output = Some(crate::tools::ToolOutput {
            content: "Appended 7 bytes".to_string(),
            is_error: false,
            meta: None,
        });
        assert!(state
            .observe_tool_batch(
                &[partial_block],
                &[partial_output],
                &["presentation.pptx".into()]
            )
            .is_none());
        assert_eq!(state.malformed_write_file_count, 2);

        let third = state
            .observe_tool_batch(
                &[malformed_block.clone()],
                &[malformed_output.clone()],
                &["presentation.pptx".into()],
            )
            .expect("malformed write after partial delivery should keep escalating");
        assert!(third.contains("malformed 3 times"));

        let valid_block = (
            "tu_ok".to_string(),
            "shell_exec".to_string(),
            serde_json::json!({"command":"python3 create_ppt.py && test -s presentation.pptx"}),
        );
        let valid_output = Some(crate::tools::ToolOutput {
            content: "ok".to_string(),
            is_error: false,
            meta: None,
        });
        assert!(state
            .observe_tool_batch(&[valid_block], &[valid_output], &[])
            .is_none());
        assert_eq!(state.malformed_write_file_count, 0);
    }

    #[test]
    fn no_tool_progress_hint_handles_hidden_reasoning_only() {
        let visible = no_tool_progress_hint(2, true);
        assert!(visible.contains("task_complete"));
        assert!(visible.contains("2 replies"));

        let hidden_only = no_tool_progress_hint(2, false);
        assert!(hidden_only.contains("no visible answer"));
        assert!(hidden_only.contains("hidden reasoning only"));
    }

    #[test]
    fn guardrail_repair_instruction_preserves_exact_feedback_and_guides_length_rewrite() {
        let feedback = "[Guardrail Check Failed]\n- command_passes ✗ FAILED: analysis_report.md length must be 600-1200 non-whitespace chars, got 1962\n- command_passes ✗ FAILED: missing required header: # 风险与建议\n- command_passes ✗ FAILED: final response facts should preserve exact source marker(s): service-level objective";
        let instruction = guardrail_repair_instruction(
            feedback,
            &["analysis_report.md".to_string(), "summary.json".to_string()],
        );
        assert!(instruction.contains("[System][Guardrail Repair]"));
        assert!(instruction.contains("analysis_report.md, summary.json"));
        assert!(instruction.contains("Length/shape repair"));
        assert!(instruction.contains("condense each section"));
        assert!(instruction.contains("Header repair"));
        assert!(instruction.contains("Source-marker repair"));
        assert!(instruction.contains("preserve all source marker substrings"));
        assert!(instruction.contains("fix only the latest missing marker"));
        assert!(instruction.contains(feedback));
    }

    #[test]
    fn shell_nested_agent_detection_blocks_recursive_cli() {
        assert!(shell_command_invokes_nested_agent(&serde_json::json!({
            "command": "claude -p 'extract facts'"
        })));
        assert!(shell_command_invokes_nested_agent(&serde_json::json!({
            "command": "npx claude --print 'delegate'"
        })));
        assert!(!shell_command_invokes_nested_agent(&serde_json::json!({
            "command": "python3 scripts/extract.py"
        })));
    }

    /// reactive_compact_aggressive 比 microcompact 更激进：drop 一半而非 1/3。
    /// 这是"已经撞墙"场景的合理取舍——多丢一些上下文换"重发能过"。
    #[test]
    fn reactive_compact_is_more_aggressive_than_microcompact() {
        let mut for_reactive: Vec<Message> = (0..9).map(|i| user_msg(&format!("m{i}"))).collect();
        let mut for_micro: Vec<Message> = (0..9).map(|i| user_msg(&format!("m{i}"))).collect();

        let r_report = reactive_compact_aggressive(&mut for_reactive).unwrap();
        let m_report = microcompact(&mut for_micro).unwrap();

        assert!(
            r_report.dropped_messages > m_report.dropped_messages,
            "reactive 必须比 proactive 更激进；got reactive={} vs micro={}",
            r_report.dropped_messages,
            m_report.dropped_messages
        );
        assert_eq!(r_report.dropped_messages, 4, "9 / 2 = 4");
        assert_eq!(m_report.dropped_messages, 3, "9 / 3 = 3");
    }

    /// 死循环防御：messages 长度刚好等于阈值 4 时压一次后还剩 3 ——
    /// 第二次调用必须返回 None。这是 caller flag 加防御的双保险。
    #[test]
    fn reactive_compact_second_call_after_min_threshold_returns_none() {
        let mut messages: Vec<Message> = (0..4).map(|i| user_msg(&format!("m{i}"))).collect();
        let first = reactive_compact_aggressive(&mut messages);
        assert!(first.is_some(), "4 条消息应能压一次");
        // 现在 messages 长度 = 4 - 2 + 1 = 3
        assert_eq!(messages.len(), 3);
        // 第二次：< 4 → None（让 caller 真 bail，不死循环）
        let second = reactive_compact_aggressive(&mut messages);
        assert!(second.is_none(), "压完后再调必须返回 None 兜底");
    }

    /// tokens 必须真的下降——这是 reactive 存在的核心价值。
    /// 如果 summary 字串本身比被 drop 的内容还大（病态情况），就要返回前压缩
    /// 没意义。当前实现没有这道防御（认为生产消息内容总比一行 summary 大）。
    /// 这条测试 codify 当前正常情况下 tokens 一定下降的契约。
    #[test]
    fn reactive_compact_actually_reduces_tokens_for_normal_payload() {
        let mut messages: Vec<Message> = Vec::new();
        for i in 0..8 {
            messages.push(user_msg(
                &format!("user message {i} with some content ").repeat(30),
            ));
        }
        let before_tokens = approximate_tokens(&messages);
        let report = reactive_compact_aggressive(&mut messages).unwrap();
        let after_tokens = approximate_tokens(&messages);
        assert!(
            after_tokens < before_tokens,
            "正常 payload 下 reactive 必须真的减少 token；got {} → {}",
            report.tokens_before,
            report.tokens_after
        );
        assert_eq!(report.tokens_before, before_tokens);
        assert_eq!(report.tokens_after, after_tokens);
    }
}

#[cfg(test)]
mod max_output_tokens_escalation_tests {
    //! Single-Agent Uplift P1-3：max_output_tokens 三档恢复的 escalation cap 计算。
    //!
    //! 三档行为本身需要 mock provider 跑集成测试（独立 PR），这里**只**守住
    //! `compute_escalated_cap` 的纯函数行为——主循环的状态机依赖它正确。
    //!
    //! 守住的不变量：
    //!   ① 大 ctx 模型 → 升档到 ESCALATED_MAX_OUTPUT_TOKENS（64K）封顶
    //!   ② 中 ctx 模型 → 升档到 context_window/2
    //!   ③ 小 ctx 模型 → 升档值 ≤ 当前值 = "不该升"（caller 据此跳过升档分支）
    //!   ④ current 已经在 ESCALATED → 永不再升

    use super::*;

    #[test]
    fn large_ctx_model_escalates_to_64k() {
        // anthropic claude-4-sonnet ctx = 200K → 200K/2 = 100K，与 64K 取 min = 64K
        let cap = compute_escalated_cap("anthropic", "claude-4-sonnet", 16_384);
        assert_eq!(cap, ESCALATED_MAX_OUTPUT_TOKENS);
    }

    #[test]
    fn medium_ctx_model_escalates_to_half_window() {
        // dashscope 未命中 qwen3 系列时走 dashscope_default → ctx=32768
        // → upper(16384).min(64K).max(8K) = 16384
        let cap = compute_escalated_cap("dashscope", "some-other-model", 8_192);
        assert_eq!(cap, 16_384, "32K ctx fallback 模型升档值 = ctx/2 = 16K");
    }

    #[test]
    fn small_ctx_model_clamps_below_current_means_no_escalation() {
        // 32K ctx 模型，caller current 已是 32K → upper(16384).max(32K) = 32K
        // 等于 current → caller 检查 escalated_cap > current 为 false → 跳过升档
        let cap = compute_escalated_cap("dashscope", "some-other-model", 32_000);
        assert_eq!(
            cap, 32_000,
            "升档值不能低于当前值；upper(ctx/2)=16K < current=32K → 取 current"
        );
    }

    #[test]
    fn already_at_64k_stays_at_64k() {
        // current 已是 64K：upper(100K).max(64K) = 64K，等于 current → caller 跳过升档
        let cap =
            compute_escalated_cap("anthropic", "claude-4-sonnet", ESCALATED_MAX_OUTPUT_TOKENS);
        assert_eq!(cap, ESCALATED_MAX_OUTPUT_TOKENS);
    }

    #[test]
    fn unknown_model_uses_default_ctx() {
        // unknown provider/model fallback 到 default ctx=32768
        let cap = compute_escalated_cap("unknown-provider", "unknown-model", 4_096);
        // default ctx=32768 → upper(16K).max(4K) = 16K
        assert_eq!(cap, 16_384);
    }

    #[test]
    fn escalated_constant_is_within_reasonable_range() {
        // 防御：常量本身被错误调小（如 8K）会让 P1-3 整个失效
        assert!(
            ESCALATED_MAX_OUTPUT_TOKENS >= 32_768,
            "ESCALATED_MAX_OUTPUT_TOKENS 太小，无法覆盖大多数撞顶场景"
        );
        // 上限 256K：超过此值多数生产模型不支持，clamp 也救不回来
        assert!(
            ESCALATED_MAX_OUTPUT_TOKENS <= 262_144,
            "ESCALATED_MAX_OUTPUT_TOKENS 太大，可能被 provider 直接 400"
        );
    }

    #[test]
    fn recovery_limit_is_within_reasonable_range() {
        // 防御：太小没意义（< 2 等于没 multi-turn），太大成本失控（> 5 累计 N×64K output）
        assert!(MAX_OUTPUT_TOKENS_RECOVERY_LIMIT >= 2);
        assert!(MAX_OUTPUT_TOKENS_RECOVERY_LIMIT <= 5);
    }
}

#[cfg(test)]
mod tool_followup_protocol_tests {
    //! **协议契约回归测试** —— DeepSeek/OpenAI tool-call 协议要求：
    //! assistant message 含 tool_calls 之后，紧跟的 follow-up user message
    //! **必须**先列 tool_results、再列任何附加 hint text。中间不允许独立的
    //! user-text message。
    //!
    //! 任何在主循环里"忘了用 builder 而直接 push user message"的回归
    //! 都会让生产环境再次撞上 `insufficient tool messages following tool_calls`
    //! 400 错误（f5866369 复现链）。守住这一组小不变量就能让重构不至于
    //! 摔进同一个坑。
    use super::*;
    use crate::llm::{LlmRequest, OpenAICompatProvider};

    fn provider() -> OpenAICompatProvider {
        OpenAICompatProvider::new("k".into(), "https://example.com".into())
    }

    /// builder 空时构造的 follow-up 等价于"只有 tool_results 的 user message"。
    #[test]
    fn followup_with_only_tool_results_yields_no_text_block() {
        let mut b = ToolFollowupBuilder::with_capacity(2);
        b.push_tool_result("id_a".into(), "ra".into(), false);
        b.push_tool_result("id_b".into(), "rb".into(), false);
        let m = b.build();

        assert!(matches!(m.role, MessageRole::User));
        assert_eq!(m.content.len(), 2, "无 hint 时不能凭空多出 Text block");
        assert!(matches!(
            &m.content[0],
            ContentBlock::ToolResult { tool_use_id, .. } if tool_use_id == "id_a"
        ));
        assert!(matches!(
            &m.content[1],
            ContentBlock::ToolResult { tool_use_id, .. } if tool_use_id == "id_b"
        ));
    }

    /// hint 必须排在 tool_results 之后——这是 OpenAI 协议合规的关键顺序。
    #[test]
    fn followup_appends_hints_after_tool_results() {
        let mut b = ToolFollowupBuilder::with_capacity(1);
        b.push_tool_result("id_a".into(), "ra".into(), false);
        b.append_hint("[System] read-only loop hint");
        b.append_hint("[directive] note 1 applied");
        let m = b.build();

        assert_eq!(m.content.len(), 2, "tool_result + 拼接的 Text block");
        assert!(matches!(&m.content[0], ContentBlock::ToolResult { .. }));
        match &m.content[1] {
            ContentBlock::Text { text } => {
                assert!(text.contains("read-only loop"));
                assert!(text.contains("directive"));
            }
            _ => panic!("hint 必须落在 Text block"),
        }
    }

    /// 空 hint 字符串不应污染 follow-up——保护调用方对边界条件的随手 push。
    #[test]
    fn empty_hint_is_skipped() {
        let mut b = ToolFollowupBuilder::with_capacity(1);
        b.push_tool_result("id_a".into(), "ra".into(), false);
        b.append_hint("");
        b.append_hint(String::new());
        let m = b.build();
        assert_eq!(m.content.len(), 1, "空 hint 不能造出空 Text block");
    }

    /// **协议合规端到端**：含 ToolResult+Text 的 user message 经
    /// `OpenAICompatProvider::convert_messages` 转换后，role=tool 必须
    /// **严格排在** role=user(text) 之前。如果顺序颠倒，DeepSeek 会以
    /// `insufficient tool messages following tool_calls message` 直接 400。
    #[test]
    fn convert_messages_emits_tool_messages_before_user_text() {
        let mut b = ToolFollowupBuilder::with_capacity(2);
        b.push_tool_result("call_a".into(), "result_a".into(), false);
        b.push_tool_result("call_b".into(), "result_b".into(), false);
        b.append_hint("[System] do something different next");
        let user_msg = b.build();

        // 模拟一轮真实对话：assistant 先发 tool_calls，user 紧跟 follow-up。
        let assistant_msg = Message {
            role: MessageRole::Assistant,
            content: vec![
                ContentBlock::ToolUse {
                    id: "call_a".into(),
                    name: "read_file".into(),
                    input: serde_json::json!({"path": "x"}),
                },
                ContentBlock::ToolUse {
                    id: "call_b".into(),
                    name: "list_files".into(),
                    input: serde_json::json!({"path": "."}),
                },
            ],
            cache_control: None,
        };
        let req = LlmRequest {
            model: "deepseek-v4-pro".into(),
            system: None,
            messages: vec![assistant_msg, user_msg],
            tools: vec![],
            max_tokens: 16,
            provider_extras: None,
        };

        let oai = provider().convert_messages(&req);

        // 期望序列：
        //   [0] assistant tool_calls
        //   [1] tool call_a
        //   [2] tool call_b
        //   [3] user text(hint)
        assert_eq!(oai.len(), 4, "应当展开成 4 条 OpenAI 协议消息: {oai:?}");
        assert_eq!(oai[0]["role"], "assistant");
        assert!(oai[0].get("tool_calls").is_some());

        assert_eq!(oai[1]["role"], "tool", "tool_results 必须紧跟 assistant");
        assert_eq!(oai[1]["tool_call_id"], "call_a");
        assert_eq!(oai[2]["role"], "tool");
        assert_eq!(oai[2]["tool_call_id"], "call_b");

        assert_eq!(
            oai[3]["role"], "user",
            "hint user message 必须排在所有 tool_results 之后（协议合规关键）"
        );
        assert!(oai[3]["content"]
            .as_str()
            .expect("user content 必须是 string")
            .contains("do something different"));
    }
}
