use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};
use tauri::Emitter;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::Command;

use super::ripgrep::{resolve_rg_command, GREP_MAX_LINE_CHARS, GREP_MAX_OUTPUT_CHARS};

/// `agent-tool-stream` 事件载荷：让前端 Workspace 视图实时拼接 shell 输出。
/// 同一 agent_id 的连续 chunk 按 stream 分流（stdout / stderr 各自一条流）。
#[derive(Debug, Clone, Serialize)]
pub struct ToolStreamPayload {
    pub agent_id: String,
    pub tool: String,
    /// "stdout" / "stderr" / "meta"（meta 用于 watchdog 终止/启动元信息）
    pub stream: String,
    pub chunk: String,
    /// true 表示该 stream 已 EOF（进程退出 / 被 kill）
    pub eof: bool,
}

/// 流式 emit 的目标上下文。仅在 `execute_with_stream` 入口注入。
#[derive(Clone)]
struct StreamCtx {
    app: tauri::AppHandle,
    agent_id: String,
}

/// 单次 `shell_exec` 输出的最大字节数（stdout / stderr 各自）。超过则保留末尾、丢弃头部，
/// 因为构建/测试类长命令的关键信息（错误堆栈、最终结论）几乎总在尾部。
const SHELL_OUTPUT_MAX_BYTES: usize = 16 * 1024;
/// reader 单次系统调用读多少字节。
const SHELL_READ_CHUNK: usize = 4096;
/// watchdog 巡检间隔。
///
/// **为什么 100ms 不更高频**：每个 tick 都做一次 `try_wait` 系统调用 +
/// last_byte_at lock 检查。100ms ≈ 10 syscalls/sec，开销可忽略。
/// **为什么不更低频**：tokio multi-thread runtime 上 `child.wait()` 偶尔
/// 错过 SIGCHLD（已知 race，tokio-rs/tokio#3520 等），需要 try_wait 兜底，
/// tick 间隔即兜底延迟上限——`mkdir -p` 这种瞬时命令直接卡住的体感很差。
const SHELL_WATCHDOG_TICK: Duration = Duration::from_millis(100);
/// 默认（短任务）：60s 无新输出 / 5min 总时长。覆盖 99% 命令。
const SHELL_DEFAULT_IDLE_SECS: u64 = 60;
const SHELL_DEFAULT_WALL_SECS: u64 = 300;
/// 长任务（agent 显式声明 `expect_long_running: true`）：120s 无输出 / 30min 总时长。
const SHELL_LONG_IDLE_SECS: u64 = 120;
const SHELL_LONG_WALL_SECS: u64 = 1800;
const READ_FILE_MAX_OUTPUT_CHARS: usize = 120 * 1024;
const READ_FILE_MAX_LINE_CHARS: usize = 4 * 1024;
const LIST_FILES_DEFAULT_LIMIT: usize = 200;
const LIST_FILES_HARD_LIMIT: usize = 1000;
const NOTEBOOK_READ_CELLS_DEFAULT_LIMIT: usize = 50;
const NOTEBOOK_READ_CELLS_HARD_LIMIT: usize = 200;
const NOTEBOOK_CELL_SOURCE_MAX_CHARS: usize = 4 * 1024;
const NOTEBOOK_READ_CELLS_TOTAL_SOURCE_CHARS: usize = 80 * 1024;
const SHELL_EVIDENCE_CAPTURE_MAX_BYTES: usize = 8 * 1024 * 1024;
const SHELL_COMPACT_THRESHOLD_CHARS: usize = 24 * 1024;
const SHELL_COMPACT_MAX_CHARS: usize = 8 * 1024;
const EVIDENCE_READ_REF_THRESHOLD_CHARS: usize = 8 * 1024;
const EVIDENCE_READ_EXCERPT_CHARS: usize = 1600;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolOutput {
    pub content: String,
    pub is_error: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<serde_json::Value>,
}

impl ToolOutput {
    fn ok(content: String) -> Self {
        Self {
            content,
            is_error: false,
            meta: None,
        }
    }

    fn error(kind: &str, message: &str) -> Self {
        Self {
            content: serde_json::json!({ "error": kind, "message": message }).to_string(),
            is_error: true,
            meta: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ToolExecutionContext {
    pub agent_id: String,
    pub step: u32,
    pub tool_use_id: String,
    pub tool_name: String,
}

fn default_evidence_root(workspace_root: &std::path::Path) -> PathBuf {
    let workspace_name = workspace_root
        .file_name()
        .and_then(|name| name.to_str())
        .map(sanitize_path_segment)
        .unwrap_or_else(|| "workspace".to_string());
    workspace_root
        .parent()
        .unwrap_or(workspace_root)
        .join(".miragenty-evidence")
        .join(workspace_name)
}

pub struct ToolExecutor {
    workspace_root: PathBuf,
    evidence_root: PathBuf,
    rg_resource_dir: Option<PathBuf>,
    /// Single-Agent Uplift Phase 1.1 + A2: 跟踪本 ToolExecutor 实例上"agent 已知最新内容"的路径
    /// 及其 mtime，用于：
    ///   1. `edit_file` 的"必须先读"前置条件（防止 LLM 凭空臆造改动）
    ///   2. `file_unchanged_since_last_read` stub —— 二次 read 时若 mtime 未变，
    ///      返回 unchanged 标记，省掉重复输出几千行的 token 浪费
    ///
    /// 用 canonical 绝对路径作为 key，保持与 `resolve_path` / `read_file` 一致；
    /// 一个 ToolExecutor 绑定一个 agent，故 in-memory map 足够，不需要持久化。
    ///
    /// **同步规则**：以下入口都必须更新这张表，否则 A2 stub 会出 stale 数据：
    ///   - `read_file` 成功后 → 写入实测 mtime
    ///   - `write_file` 成功后 → 写入实测 mtime（既消除"写完不能 edit"的假阳性，
    ///     又让 A2 在下次 read 时正确识别"内容是 agent 自己写的，无需重读"）
    ///   - `edit_file` 成功后 → 写入新的实测 mtime（避免 stale）
    ///
    /// **shell_exec 边角**：通过 shell 修改文件 → 这里不会自动更新；
    /// 但 A2 stub 走的是"实测当前 mtime vs 缓存 mtime"对比，shell 修改后 mtime 会变，
    /// 自然 fall-through 到完整 read，**没有 stale 风险**。
    read_paths: Arc<Mutex<HashMap<PathBuf, SystemTime>>>,
    /// 可选取消信号。None = 不响应取消（chat 入口、单测）。
    /// AgentEngine 注入自身的 cancel_token 后，shell_exec 的 watchdog 会监听这个
    /// 信号 —— 用户点"停止"时立即 SIGTERM 子进程，而不是等 wall_secs / idle_secs 兜底。
    cancel_token: Option<tokio_util::sync::CancellationToken>,
}

impl ToolExecutor {
    pub fn new(workspace_root: PathBuf) -> Self {
        let workspace_root = workspace_root.canonicalize().unwrap_or(workspace_root);
        Self {
            evidence_root: default_evidence_root(&workspace_root),
            workspace_root,
            rg_resource_dir: None,
            read_paths: Arc::new(Mutex::new(HashMap::new())),
            cancel_token: None,
        }
    }

    /// Builder: override where shell evidence files are stored. The directory may be outside
    /// the workspace so internal traces do not contaminate user-visible file scans.
    pub fn with_evidence_root(mut self, evidence_root: PathBuf) -> Self {
        self.evidence_root = evidence_root;
        self
    }

    /// Builder: provide Tauri's resource directory so packaged builds can prefer bundled tools.
    pub fn with_rg_resource_dir(mut self, resource_dir: Option<PathBuf>) -> Self {
        self.rg_resource_dir = resource_dir;
        self
    }

    /// Builder：注入 cancel_token，让 shell_exec 等长跑工具能响应用户取消。
    /// AgentEngine 在创建 ToolExecutor 后立刻调一次。
    pub fn with_cancel_token(mut self, token: tokio_util::sync::CancellationToken) -> Self {
        self.cancel_token = Some(token);
        self
    }

    /// 记录或刷新某个 canonical 路径的 mtime。
    /// 任何"agent 写的字节即将落盘 / 已落盘"或"agent 刚刚读过"的入口都应调用。
    fn record_path_mtime(&self, canonical: PathBuf) {
        let mtime = std::fs::metadata(&canonical)
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        self.read_paths.lock().unwrap().insert(canonical, mtime);
    }

    fn attach_persisted_tool_result(
        &self,
        mut output: ToolOutput,
        exec_ctx: Option<&ToolExecutionContext>,
    ) -> ToolOutput {
        const PERSIST_TOOL_RESULT_THRESHOLD_CHARS: usize = 2 * 1024;
        if output.is_error || output.content.chars().count() <= PERSIST_TOOL_RESULT_THRESHOLD_CHARS
        {
            return output;
        }
        let Some(ctx) = exec_ctx else {
            return output;
        };
        let dir = self
            .evidence_root
            .join(sanitize_path_segment(&ctx.agent_id))
            .join(format!("step-{:04}", ctx.step))
            .join(sanitize_path_segment(&ctx.tool_use_id));
        if let Err(err) = std::fs::create_dir_all(&dir) {
            tracing::warn!(
                agent_id = %ctx.agent_id,
                step = ctx.step,
                tool_use_id = %ctx.tool_use_id,
                error = %err,
                "failed to create persisted tool result directory"
            );
            return output;
        }
        let path = dir.join("tool-result.txt");
        if let Err(err) = std::fs::write(&path, output.content.as_bytes()) {
            tracing::warn!(
                agent_id = %ctx.agent_id,
                step = ctx.step,
                tool_use_id = %ctx.tool_use_id,
                error = %err,
                "failed to persist tool result output"
            );
            return output;
        }
        let display_path = path_display_for_agent(&self.workspace_root, &self.evidence_root, &path);
        let meta = serde_json::json!({
            "kind": "persisted_tool_result",
            "persisted_path": display_path,
            "tool_result_path": display_path,
            "tool_result_bytes": output.content.len(),
            "tool_result_chars": output.content.chars().count(),
            "tool_result_lines": output.content.lines().count(),
            "content_source": content_source_from_tool_context(&ctx.tool_name, &output),
            "content_shape": classify_text_shape(&output.content),
        });
        output.meta = Some(merge_tool_meta(output.meta.take(), meta));
        output
    }
    /// 查询 canonical 路径上次记录的 mtime（read/write/edit 任一）。
    fn last_known_mtime(&self, canonical: &PathBuf) -> Option<SystemTime> {
        self.read_paths.lock().unwrap().get(canonical).copied()
    }

    fn has_been_seen(&self, canonical: &PathBuf) -> bool {
        self.read_paths.lock().unwrap().contains_key(canonical)
    }

    pub fn workspace_display(&self) -> String {
        self.workspace_root.display().to_string()
    }

    pub async fn execute(&self, tool_name: &str, input: &serde_json::Value) -> ToolOutput {
        match tool_name {
            "read_file" => self.read_file(input).await,
            "write_file" => self.write_file(input).await,
            "edit_file" => self.edit_file(input).await,
            // 2026-05 重命名 `search_files` → `grep`；`search_files` 保留为 alias，
            // 兼容旧 session replay 与历史 hook config。canonical 主路径见 `tools::registry`。
            "grep" | "search_files" => self.grep(input).await,
            "glob" => self.glob_files(input).await,
            "notebook_edit" => self.notebook_edit(input).await,
            "shell_exec" => self.shell_exec(input, None, None).await,
            "list_files" => self.list_files(input).await,
            _ => ToolOutput::error("unknown_tool", &format!("Unknown tool: {tool_name}")),
        }
    }

    /// 与 [`Self::execute`] 等价，但对 `shell_exec` 额外把 stdout/stderr 增量
    /// emit 为 `agent-tool-stream` 事件，供前端 Workspace 视图实时展示。
    /// 其它工具透传到 `execute`，行为不变。
    pub async fn execute_with_stream(
        &self,
        tool_name: &str,
        input: &serde_json::Value,
        app: &tauri::AppHandle,
        agent_id: &str,
    ) -> ToolOutput {
        self.execute_with_stream_context(tool_name, input, app, agent_id, None)
            .await
    }

    pub async fn execute_with_stream_context(
        &self,
        tool_name: &str,
        input: &serde_json::Value,
        app: &tauri::AppHandle,
        agent_id: &str,
        exec_ctx: Option<ToolExecutionContext>,
    ) -> ToolOutput {
        if tool_name == "shell_exec" {
            self.shell_exec(
                input,
                Some(StreamCtx {
                    app: app.clone(),
                    agent_id: agent_id.to_string(),
                }),
                exec_ctx,
            )
            .await
        } else {
            let output = self.execute(tool_name, input).await;
            self.attach_persisted_tool_result(output, exec_ctx.as_ref())
        }
    }

    /// Single-Agent Uplift A1 + A2: read_file 升级版。
    ///
    /// 行为变化（vs 上一版）：
    /// - **行号前缀**：每行输出 `LINE_NUMBER|CONTENT`，6 位右对齐。让 LLM 在写
    ///   `edit_file.old_string` 时能精确知道在哪一行；同时 `edit_file` 入口会
    ///   兜底 strip 这种前缀（B4 的一部分），即便 LLM 把 "  42|foo" 复制粘贴
    ///   过去，也不会因为多了行号而 no_match。
    /// - **offset/limit 分页**：`offset >= 1` 表示从第几行开始（1-indexed），
    ///   `limit` 表示读多少行。两者都没传时整文件读取。文件超过 2000 行时
    ///   自动 limit=2000 + 提示 "use offset to continue"，避免单次 tool_result
    ///   动辄塞进 60K token。
    /// - **A2 unchanged stub**：当 agent 在没有 offset/limit 的情况下重读
    ///   一个 mtime 自上次 read/write/edit 以来未变的文件，直接返回
    ///   `<file_unchanged_since_last_read path=... mtime=... lines=...>` 占位，
    ///   不重传内容。LLM 知道"那次读到的内容仍然有效"。innerCC 的 stub 行为
    ///   等价。带 offset/limit 的精确分页请求不走 stub（用户可能是为了拿不
    ///   同窗口）。
    /// - **二进制兜底**：检测到 NUL 字节时返回结构化错误，不 lossy UTF-8。
    /// - **大文件**：超过 4MiB 的文件直接拒绝，提示用 grep（ripgrep）。
    async fn read_file(&self, input: &serde_json::Value) -> ToolOutput {
        let rel_path = match input["path"].as_str() {
            Some(p) => p,
            None => {
                return ToolOutput::error(
                    "parameter_error",
                    &format!(
                        "Missing 'path' parameter. Received fields: {}",
                        summarize_json_value_for_error(input)
                    ),
                )
            }
        };
        let offset = input.get("offset").and_then(|v| v.as_u64());
        let limit = input.get("limit").and_then(|v| v.as_u64());
        let is_paged_request = offset.is_some() || limit.is_some();

        let full_path = match self.resolve_path(rel_path) {
            Ok(p) => p,
            Err(e) => return ToolOutput::error("sandbox_violation", &e.to_string()),
        };
        let canonical = full_path
            .canonicalize()
            .unwrap_or_else(|_| full_path.clone());

        // 读 metadata；NotFound 单独处理。
        let metadata = match tokio::fs::metadata(&full_path).await {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return ToolOutput::error("file_not_found", &format!("File not found: {rel_path}"));
            }
            Err(e) => return ToolOutput::error("io_error", &e.to_string()),
        };
        if metadata.is_dir() {
            return ToolOutput::error(
                "is_directory",
                &format!("`{rel_path}` is a directory; use list_files / glob instead."),
            );
        }
        const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;
        if metadata.len() > MAX_FILE_BYTES {
            return ToolOutput::error(
                "file_too_large",
                &format!(
                    "{rel_path} is {} bytes (>4MiB). Use grep to search within it, or pass \
                     offset+limit to page through it.",
                    metadata.len()
                ),
            );
        }
        let current_mtime = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);

        let canonical_for_scope = full_path
            .canonicalize()
            .unwrap_or_else(|_| Self::normalize_lexical(&full_path));
        if !is_paged_request && matches!(self.path_scope(&canonical_for_scope), PathScope::Evidence)
        {
            let bytes = match tokio::fs::read(&full_path).await {
                Ok(b) => b,
                Err(e) => return ToolOutput::error("io_error", &e.to_string()),
            };
            if bytes.contains(&0u8) {
                return ToolOutput::error(
                    "binary_file",
                    &format!("{rel_path} appears to be a binary evidence file."),
                );
            }
            let content = String::from_utf8_lossy(&bytes).into_owned();
            if content.chars().count() > EVIDENCE_READ_REF_THRESHOLD_CHARS {
                let rendered = render_evidence_read_ref(
                    rel_path,
                    &content,
                    metadata.len(),
                    content.lines().count(),
                );
                return ToolOutput {
                    content: rendered,
                    is_error: false,
                    meta: Some(serde_json::json!({
                        "kind": "evidence_read_ref",
                        "path": rel_path,
                        "content_source": rel_path,
                        "content_shape": classify_text_shape(&content),
                        "size_bytes": metadata.len(),
                        "lines": content.lines().count(),
                    })),
                };
            }
        }

        // A2 stub：仅对"无 offset/limit"的整文件重读启用。
        if !is_paged_request {
            if let Some(known) = self.last_known_mtime(&canonical) {
                if known == current_mtime && known != SystemTime::UNIX_EPOCH {
                    let line_count = metadata.len(); // bytes 上限近似；细节在 stub 里说明
                    let stub = serde_json::json!({
                        "file_unchanged_since_last_read": true,
                        "path": rel_path,
                        "size_bytes": line_count,
                        "hint": "Content has not changed since you last read/wrote this file. \
                                Reuse the previous read output. If you suspect external changes, \
                                pass force=true (not yet implemented) or read with offset+limit.",
                    });
                    return ToolOutput::ok(stub.to_string());
                }
            }
        }

        let bytes = match tokio::fs::read(&full_path).await {
            Ok(b) => b,
            Err(e) => return ToolOutput::error("io_error", &e.to_string()),
        };
        // 二进制检测：NUL 字节出现 = 几乎肯定不是文本文件。innerCC 同款启发式。
        if bytes.contains(&0u8) {
            return ToolOutput::error(
                "binary_file",
                &format!(
                    "{rel_path} appears to be a binary file (contains NUL bytes). Use \
                     `file` / `xxd` via shell_exec if you really need to inspect it."
                ),
            );
        }
        let content = match String::from_utf8(bytes) {
            Ok(s) => s,
            Err(e) => {
                // 非严格 UTF-8：用 lossy 但提示。
                String::from_utf8_lossy(e.as_bytes()).into_owned()
            }
        };

        // 行号 + offset/limit。
        const DEFAULT_AUTO_LIMIT: usize = 2000;
        const HARD_MAX_LIMIT: usize = 5000;
        let lines: Vec<&str> = content.lines().collect();
        let total_lines = lines.len();
        let raw_start_idx: usize = match offset {
            Some(o) if o >= 1 => (o as usize).saturating_sub(1),
            _ => 0,
        };
        let requested_limit: usize = match limit {
            Some(l) => (l as usize).min(HARD_MAX_LIMIT),
            None => {
                if is_paged_request {
                    HARD_MAX_LIMIT
                } else {
                    DEFAULT_AUTO_LIMIT
                }
            }
        };
        let start_idx = raw_start_idx.min(total_lines);
        let end_idx = (start_idx + requested_limit).min(total_lines);
        let truncated_head = raw_start_idx > 0;
        let truncated_tail = end_idx < total_lines;

        let mut rendered = String::with_capacity(content.len() + total_lines * 8);
        if raw_start_idx >= total_lines && total_lines > 0 {
            rendered.push_str(&format!(
                "[requested offset {} is beyond end of file; file has {} lines]\n",
                raw_start_idx + 1,
                total_lines
            ));
        } else if truncated_head {
            rendered.push_str(&format!(
                "[skipped lines 1..{}; pass offset=1 to start from top]\n",
                start_idx
            ));
        }
        for (i, line) in lines[start_idx..end_idx].iter().enumerate() {
            let line_num = start_idx + i + 1;
            let rendered_line = truncate_middle_chars(line, READ_FILE_MAX_LINE_CHARS);
            rendered.push_str(&format!("{:>6}|{}\n", line_num, rendered_line));
        }
        if truncated_tail {
            rendered.push_str(&format!(
                "[truncated at line {} of {}; pass offset={} (and optionally limit) to continue]\n",
                end_idx,
                total_lines,
                end_idx + 1,
            ));
        }

        // 完整读（无 paging）才更新 mtime cache；分页读不刷新——避免后续整体 read 的 stub 误判。
        if !is_paged_request {
            self.read_paths
                .lock()
                .unwrap()
                .insert(canonical, current_mtime);
        } else {
            // 分页时也至少登记"看过"，让 edit_file 不再卡 read precondition（但 mtime 用占位值
            // 表示"未必最新"，下次整体读会失效 stub）。
            self.read_paths
                .lock()
                .unwrap()
                .entry(canonical)
                .or_insert(SystemTime::UNIX_EPOCH);
        }
        let rendered = cap_text_with_notice(
            rendered,
            READ_FILE_MAX_OUTPUT_CHARS,
            "use a smaller limit/offset window or grep for a narrower pattern",
        );
        ToolOutput {
            content: rendered,
            is_error: false,
            meta: Some(serde_json::json!({
                "kind": "read_file_output",
                "path": rel_path,
                "content_source": rel_path,
                "content_shape": classify_text_shape(&content),
                "size_bytes": metadata.len(),
                "lines": total_lines,
                "truncated": truncated_head || truncated_tail,
            })),
        }
    }

    /// Single-Agent Uplift Phase 1.1 + B3 + B4 + A1: 精确字符串替换。
    ///
    /// 入参支持两种 shape：
    ///   1. **单 edit**（向后兼容）：`{path, old_string, new_string, replace_all?}`
    ///   2. **multi-edit**（B3）：`{path, edits: [{old_string, new_string, replace_all?}, ...]}`
    ///      —— 数组顺序原子应用，任一条失败整体回滚（不写盘）。第 N 条的查找基于第 N-1 条
    ///      已应用后的中间内容，innerCC 同款语义。
    ///
    /// 不变量（任意一条违反就 is_error=true，不写盘）：
    /// - 必须先 read_file 同一路径（防止 LLM 凭空臆造）
    /// - 每条 edit 的 old_string 在当前内容中出现 == 1 次（除非 replace_all=true 时 ≥ 1 次）
    /// - 写盘前最终内容不能让文件变成完全空白（误删兜底）
    ///
    /// **B4 兼容性兜底**（按顺序尝试，命中即用）：
    /// 1. 原样匹配
    /// 2. 把 old_string 里的 `LINE_NUM|` 行号前缀 strip 后匹配（agent 从行号化 read 输出复制）
    /// 3. 把双方的 curly quotes (` ` " " ' ' ` ` ）映射回 ASCII 后匹配（某些模型自动美化）
    /// 4. 把 NBSP 等"看不见的等价空白"标准化后匹配
    /// 命中后 new_string 走同样的 strip/normalize，保证写入字节是 LLM 期望的语义。
    async fn edit_file(&self, input: &serde_json::Value) -> ToolOutput {
        let rel_path = match input["path"].as_str() {
            Some(p) => p,
            None => {
                return ToolOutput::error(
                    "parameter_error",
                    &format!(
                        "Missing 'path' parameter. Received fields: {}",
                        summarize_json_value_for_error(input)
                    ),
                )
            }
        };

        // 解析 edits 列表：multi-edit 优先，否则单 edit fallback。
        struct EditOp {
            old_string: String,
            new_string: String,
            replace_all: bool,
        }
        let mut edits: Vec<EditOp> = Vec::new();
        if let Some(arr) = input.get("edits").and_then(|v| v.as_array()) {
            if arr.is_empty() {
                return ToolOutput::error(
                    "parameter_error",
                    "`edits` is an empty array. Pass at least one edit, or use the single-edit \
                     shape (old_string + new_string).",
                );
            }
            for (i, item) in arr.iter().enumerate() {
                let old_s = match item.get("old_string").and_then(|v| v.as_str()) {
                    Some(s) => s.to_string(),
                    None => {
                        return ToolOutput::error(
                            "parameter_error",
                            &format!("edits[{i}] missing 'old_string'."),
                        )
                    }
                };
                let new_s = match item.get("new_string").and_then(|v| v.as_str()) {
                    Some(s) => s.to_string(),
                    None => {
                        return ToolOutput::error(
                            "parameter_error",
                            &format!(
                                "edits[{i}] missing 'new_string' (pass empty string to delete)."
                            ),
                        )
                    }
                };
                let ra = item
                    .get("replace_all")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                edits.push(EditOp {
                    old_string: old_s,
                    new_string: new_s,
                    replace_all: ra,
                });
            }
        } else {
            let old_s =
                match input["old_string"].as_str() {
                    Some(s) => s.to_string(),
                    None => return ToolOutput::error(
                        "parameter_error",
                        "Missing 'old_string' parameter (or pass an `edits` array for multi-edit).",
                    ),
                };
            let new_s = match input["new_string"].as_str() {
                Some(s) => s.to_string(),
                None => {
                    return ToolOutput::error(
                        "parameter_error",
                        "Missing 'new_string' parameter (pass empty string to delete).",
                    )
                }
            };
            let ra = input
                .get("replace_all")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            edits.push(EditOp {
                old_string: old_s,
                new_string: new_s,
                replace_all: ra,
            });
        }

        let full_path = match self.resolve_path(rel_path) {
            Ok(p) => p,
            Err(e) => return ToolOutput::error("sandbox_violation", &e.to_string()),
        };
        let canonical = full_path
            .canonicalize()
            .unwrap_or_else(|_| full_path.clone());

        if !self.has_been_seen(&canonical) {
            return ToolOutput::error(
                "edit_without_read",
                &format!(
                    "edit_file refused: you must call read_file on `{rel_path}` first so you can \
                     see the exact contents. Read the file, then retry edit_file with old_string \
                     copied verbatim from the read output."
                ),
            );
        }

        let original = match tokio::fs::read_to_string(&full_path).await {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return ToolOutput::error("file_not_found", &format!("File not found: {rel_path}"));
            }
            Err(e) => return ToolOutput::error("io_error", &e.to_string()),
        };

        // 顺序应用每条 edit；失败立即返回（已应用的修改尚未写盘，所以"原子"语义自动保证）。
        let mut current = original.clone();
        let mut total_replacements: u64 = 0;
        for (i, op) in edits.iter().enumerate() {
            match apply_single_edit(&current, &op.old_string, &op.new_string, op.replace_all) {
                Ok((updated, replacements, fallback_used)) => {
                    let _ = fallback_used; // 仅作为后续 meta 字段，目前不外露
                    current = updated;
                    total_replacements += replacements;
                }
                Err(e) => {
                    let prefix = if edits.len() > 1 {
                        format!("edits[{i}] failed: ")
                    } else {
                        String::new()
                    };
                    return ToolOutput::error(&e.kind, &format!("{prefix}{}", e.message));
                }
            }
        }

        if current.trim().is_empty() && !original.trim().is_empty() {
            return ToolOutput::error(
                "edit_would_blank_file",
                &format!(
                    "edit_file refused: the resulting {rel_path} would be empty. If you intended \
                     to delete the file, use a shell_exec with `rm` instead."
                ),
            );
        }

        let old_lines = original.lines().count() as i64;
        let new_lines = current.lines().count() as i64;
        let lines_added = (new_lines - old_lines).max(0) as u64;
        let lines_removed = (old_lines - new_lines).max(0) as u64;

        if let Err(e) = tokio::fs::write(&full_path, &current).await {
            return ToolOutput::error("io_error", &e.to_string());
        }
        // 关键：写完后刷新 mtime cache；下一次 read_file 走 A2 stub，省 token；
        // 同时让 has_been_seen 持续为 true，避免下一次 edit 的 edit_without_read 假阳性。
        self.record_path_mtime(canonical);

        let payload = serde_json::json!({
            "path": rel_path,
            "replacements": total_replacements,
            "edits_applied": edits.len(),
            "lines_added": lines_added,
            "lines_removed": lines_removed,
        });
        ToolOutput::ok(payload.to_string())
    }

    async fn write_file(&self, input: &serde_json::Value) -> ToolOutput {
        if input
            .get("__tool_use_input_compacted__")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            return ToolOutput::error(
                "parameter_error",
                "This is a compacted historical tool_use input stub, not executable arguments. Re-create the write_file call with explicit path/content fields instead of copying the history stub.",
            );
        }
        let rel_path = match input["path"]
            .as_str()
            .or_else(|| input["file_path"].as_str())
        {
            Some(p) => p,
            None => {
                return ToolOutput::error(
                    "parameter_error",
                    &format!(
                        "Missing 'path' parameter. Received fields: {}",
                        summarize_json_value_for_error(input)
                    ),
                )
            }
        };
        let content = match input["content"].as_str() {
            Some(c) => c,
            None => {
                return ToolOutput::error(
                    "parameter_error",
                    &format!(
                        "Missing 'content' parameter. Received fields: {}",
                        summarize_json_value_for_error(input)
                    ),
                )
            }
        };
        let append = input
            .get("append")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let full_path = match self.resolve_path(rel_path) {
            Ok(p) => p,
            Err(e) => return ToolOutput::error("sandbox_violation", &e.to_string()),
        };

        if let Some(parent) = full_path.parent() {
            if let Err(e) = tokio::fs::create_dir_all(parent).await {
                return ToolOutput::error("io_error", &e.to_string());
            }
        }
        let result = if append {
            use tokio::io::AsyncWriteExt;
            match tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&full_path)
                .await
            {
                Ok(mut file) => file.write_all(content.as_bytes()).await,
                Err(e) => Err(e),
            }
        } else {
            tokio::fs::write(&full_path, content).await
        };
        match result {
            Ok(()) => {
                // 关键修复：写完后必须把 canonical 路径登记进 read_paths，否则
                // agent 接下来想 edit_file 这个文件会被 edit_without_read 拒绝，
                // 强迫它再读一遍刚刚自己写的文件——纯浪费 token。
                let canonical = full_path.canonicalize().unwrap_or(full_path);
                self.record_path_mtime(canonical);
                if append {
                    ToolOutput::ok(format!("Appended {} bytes to {rel_path}", content.len()))
                } else {
                    ToolOutput::ok(format!("Written {} bytes to {rel_path}", content.len()))
                }
            }
            Err(e) => ToolOutput::error("io_error", &e.to_string()),
        }
    }

    /// Single-Agent Uplift Phase 1.3: 文件名 glob 匹配。
    ///
    /// 设计要点：
    /// - 用 `glob` crate 的 `**` 支持，足以覆盖 innerCC GlobTool 90% 用例；不引入 globwalk 依赖。
    /// - 结果按 mtime 降序（最新改的在前），符合用户在大 repo 里"改过的文件最相关"的直觉。
    /// - 默认 limit=100，最大 500——避免 LLM 被几万个文件淹没 context。
    /// - sandbox：路径必须落在 workspace 内，绝对路径前缀防穿越。
    async fn glob_files(&self, input: &serde_json::Value) -> ToolOutput {
        let pattern = match input["pattern"].as_str() {
            Some(p) => p,
            None => {
                return ToolOutput::error(
                    "parameter_error",
                    "Missing 'pattern' parameter (e.g. `**/*.rs`).",
                )
            }
        };
        let base_rel = input["path"].as_str().unwrap_or(".");
        let explicit_path = input["path"].as_str().is_some();
        let base = match self.resolve_path(base_rel) {
            Ok(p) => p,
            Err(e) => return ToolOutput::error("sandbox_violation", &e.to_string()),
        };
        let limit = input
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(100)
            .min(500) as usize;

        // 拼接 base/pattern。glob 接受任何路径形式，绝对相对都行。
        let full_pattern = base.join(pattern);
        let pattern_str = match full_pattern.to_str() {
            Some(s) => s,
            None => {
                return ToolOutput::error("parameter_error", "Pattern path contains non-UTF8 bytes")
            }
        };

        let walk = match glob::glob(pattern_str) {
            Ok(it) => it,
            Err(e) => {
                return ToolOutput::error("parameter_error", &format!("invalid glob pattern: {e}"))
            }
        };

        let base_scope = self.path_scope(
            &base
                .canonicalize()
                .unwrap_or_else(|_| Self::normalize_lexical(&base)),
        );
        let allowed_root = match base_scope {
            PathScope::Evidence => self
                .evidence_root
                .canonicalize()
                .unwrap_or_else(|_| Self::normalize_lexical(&self.evidence_root)),
            _ => self
                .workspace_root
                .canonicalize()
                .unwrap_or_else(|_| self.workspace_root.clone()),
        };

        let mut entries: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
        for path in walk.flatten() {
            // sandbox 兜底：glob 不会自己越界，但符号链接可能；显式校验。
            let canonical = path.canonicalize().unwrap_or_else(|_| path.clone());
            if !canonical.starts_with(&allowed_root) {
                continue;
            }
            if !explicit_path
                && !matches!(base_scope, PathScope::Evidence)
                && is_internal_evidence_or_persisted_path(&canonical, &self.workspace_root)
            {
                continue;
            }
            // 只列文件，不列目录——和 GlobTool 语义一致；LLM 列目录用 list_files。
            let mtime = match tokio::fs::metadata(&path).await {
                Ok(meta) if meta.is_file() => {
                    meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH)
                }
                _ => continue,
            };
            entries.push((mtime, path));
        }
        // 按 mtime 降序——最新动过的排前。
        entries.sort_by(|a, b| b.0.cmp(&a.0));
        let truncated = entries.len() > limit;
        let total = entries.len();
        entries.truncate(limit);

        // 输出相对路径让 LLM 可以直接喂给 read_file/edit_file。
        let mut lines: Vec<String> = entries
            .iter()
            .map(|(_, path)| self.display_path(&path))
            .collect();
        if truncated {
            lines.push(format!(
                "... [glob truncated: {total} total matches, showing newest {limit}; use a narrower pattern/path or raise limit up to 500]"
            ));
        }
        if lines.is_empty() {
            ToolOutput::ok(format!("No files matched `{pattern}` under `{base_rel}`."))
        } else {
            ToolOutput::ok(lines.join("\n"))
        }
    }

    /// `grep` 工具实现（历史名 `search_files`，2026-05 重命名）。底层 ripgrep。
    ///
    /// 入参（兼容旧 schema 的 `pattern` + `path`，新增以下可选字段）：
    ///   - `pattern` (str, 必填)：ripgrep 正则
    ///   - `path` (str)：限定搜索的目录或单文件，默认 workspace 根
    ///   - `glob` (str)：路径过滤，e.g. `"*.rs"`、`"!**/target/**"`
    ///   - `type` (str)：rg --type，e.g. `"rust"`、`"py"`
    ///   - `case_insensitive` (bool)：rg -i
    ///   - `multiline` (bool)：rg -U（让 . 匹配换行）
    ///   - `output_mode` ("content"|"files_with_matches"|"count")：默认 "content"
    ///   - `context_before` (u64)：rg -B
    ///   - `context_after` (u64)：rg -A
    ///   - `context` (u64)：rg -C（同时 before/after）
    ///   - `head_limit` (u64)：截取最终输出的前 N 行（content 模式 = 总匹配数；
    ///     files_with_matches/count = 文件数）
    ///
    /// 与旧版的关键差别：
    ///   - 旧版 `--max-count 50` 是**单文件**最多 50 处，无总量限制 → 大 repo 会
    ///     输出几万行把 LLM context 撑爆。新版默认无单文件限制，但靠 head_limit
    ///     做总量截断（默认 200 行）。
    ///   - 旧版彻底丢弃 stderr → rg 报错时 LLM 拿到空字符串还以为"无匹配"。
    ///     新版区分 exit code：1=no match（OK 返回提示），2+=真正错误（带 stderr）。
    async fn grep(&self, input: &serde_json::Value) -> ToolOutput {
        let pattern = match input["pattern"].as_str() {
            Some(p) => p,
            None => {
                return ToolOutput::error(
                    "parameter_error",
                    &format!(
                        "Missing 'pattern' parameter. Received fields: {}",
                        summarize_json_value_for_error(input)
                    ),
                )
            }
        };
        let search_path = match input["path"].as_str() {
            Some(p) => match self.resolve_path(p) {
                Ok(path) => path,
                Err(e) => return ToolOutput::error("sandbox_violation", &e.to_string()),
            },
            None => self.workspace_root.clone(),
        };
        let explicit_path = input["path"].as_str().is_some();

        let glob_pat = input.get("glob").and_then(|v| v.as_str());
        let type_filter = input.get("type").and_then(|v| v.as_str());
        let case_insensitive = input
            .get("case_insensitive")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let multiline = input
            .get("multiline")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let context_before = input.get("context_before").and_then(|v| v.as_u64());
        let context_after = input.get("context_after").and_then(|v| v.as_u64());
        let context = input.get("context").and_then(|v| v.as_u64());
        let head_limit = input
            .get("head_limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(200)
            .max(1) as usize;
        let output_mode = input
            .get("output_mode")
            .and_then(|v| v.as_str())
            .unwrap_or("content");

        let mut args: Vec<String> = Vec::new();
        match output_mode {
            "files_with_matches" => {
                args.push("--files-with-matches".into());
            }
            "count" => {
                args.push("--count-matches".into());
            }
            "content" => {
                args.push("--line-number".into());
                if let Some(c) = context {
                    args.push(format!("-C{c}"));
                } else {
                    if let Some(b) = context_before {
                        args.push(format!("-B{b}"));
                    }
                    if let Some(a) = context_after {
                        args.push(format!("-A{a}"));
                    }
                }
            }
            other => {
                return ToolOutput::error(
                    "parameter_error",
                    &format!(
                        "Unknown output_mode `{other}`. Use one of: content, files_with_matches, count."
                    ),
                );
            }
        }
        if case_insensitive {
            args.push("-i".into());
        }
        if multiline {
            args.push("--multiline".into());
            args.push("--multiline-dotall".into());
        }
        if let Some(g) = glob_pat {
            args.push("--glob".into());
            args.push(g.into());
        }
        if let Some(t) = type_filter {
            args.push("--type".into());
            args.push(t.into());
        }
        if !explicit_path && !matches!(self.path_scope(&search_path), PathScope::Evidence) {
            args.push("--glob".into());
            args.push("!.miragenty-evidence/**".into());
            args.push("--glob".into());
            args.push("!.miragenty/tool-results/**".into());
            if let Some(workspace_name) = self
                .workspace_root
                .file_name()
                .and_then(|name| name.to_str())
            {
                args.push("--glob".into());
                args.push(format!("!assets/{workspace_name}/**"));
            }
        }
        // 始终 color=never，避免前端拿到 ANSI 控制字符。
        args.push("--color=never".into());
        // 用 -e 把 pattern 当字面参数传，避免 pattern 以 `-` 开头被当 flag。
        args.push("-e".into());
        args.push(pattern.into());

        let rg_command = resolve_rg_command(self.rg_resource_dir.clone());
        tracing::debug!(
            tool = "grep",
            rg_path = %rg_command.path.display(),
            rg_source = ?rg_command.source,
            workspace = %search_path.display(),
            "spawning ripgrep"
        );

        let mut child = match Command::new(&rg_command.path)
            .args(&args)
            .current_dir(&search_path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return ToolOutput::error(
                    "dependency_missing",
                    &format!(
                        "grep requires bundled ripgrep or ripgrep (`rg`) on PATH; failed to launch rg: {e}. Run `node scripts/fetch-rg.mjs` before packaging or install rg for development."
                    ),
                );
            }
            Err(e) => {
                return capability_tool_error(
                    "grep",
                    "dependency_spawn_failure",
                    format!("failed to spawn rg at {}: {e}", rg_command.path.display()),
                )
            }
        };
        let Some(stdout) = child.stdout.take() else {
            return ToolOutput::error("rg_error", "failed to capture rg stdout");
        };
        let Some(mut stderr) = child.stderr.take() else {
            return ToolOutput::error("rg_error", "failed to capture rg stderr");
        };
        let stdout_task = tokio::spawn(async move {
            let mut reader = BufReader::new(stdout).lines();
            let mut lines = Vec::new();
            let mut truncated = false;
            while let Some(line) = reader.next_line().await? {
                if lines.len() < head_limit {
                    lines.push(line);
                } else {
                    truncated = true;
                    break;
                }
            }
            Ok::<_, std::io::Error>((lines, truncated))
        });
        let stderr_task = tokio::spawn(async move {
            let mut buf = String::new();
            stderr.read_to_string(&mut buf).await?;
            Ok::<_, std::io::Error>(buf)
        });

        let (lines, truncated) = match stdout_task.await {
            Ok(Ok(result)) => result,
            Ok(Err(e)) => {
                return ToolOutput::error("rg_error", &format!("failed reading rg stdout: {e}"))
            }
            Err(e) => return ToolOutput::error("rg_error", &format!("rg stdout task failed: {e}")),
        };
        if truncated {
            let _ = child.kill().await;
        }
        let status = match child.wait().await {
            Ok(status) => status,
            Err(e) => return ToolOutput::error("rg_error", &format!("failed waiting for rg: {e}")),
        };
        let stderr = match stderr_task.await {
            Ok(Ok(stderr)) => stderr,
            Ok(Err(e)) => {
                return ToolOutput::error("rg_error", &format!("failed reading rg stderr: {e}"))
            }
            Err(e) => return ToolOutput::error("rg_error", &format!("rg stderr task failed: {e}")),
        };
        let exit_code = status.code().unwrap_or(-1);

        // rg 退出码：0=有匹配 1=无匹配 2+=真正的错误。截断时会主动 kill，
        // 此时 status 可能是 signal/nonzero，但已成功取得 head_limit 行。
        if !truncated && exit_code == 1 {
            return ToolOutput::ok(format!(
                "No matches for pattern `{pattern}`{}.",
                glob_pat
                    .map(|g| format!(" (glob `{g}`)"))
                    .unwrap_or_default()
            ));
        }
        if !truncated && exit_code >= 2 {
            return ToolOutput::error(
                "rg_error",
                &format!("ripgrep exited with code {exit_code}: {}", stderr.trim()),
            );
        }

        let mut body = lines
            .iter()
            .map(|line| truncate_middle_chars(line, GREP_MAX_LINE_CHARS))
            .collect::<Vec<_>>()
            .join("\n");
        if truncated {
            body.push_str(&format!(
                "\n... [truncated after {head_limit} lines; pass head_limit higher or narrow with glob/type]"
            ));
        }
        if body.is_empty() {
            body = format!("(rg returned no output for `{pattern}`)");
        }
        body = cap_text_with_notice(
            body,
            GREP_MAX_OUTPUT_CHARS,
            "use a narrower pattern, glob/type filter, output_mode=count/files_with_matches, or lower head_limit",
        );
        ToolOutput::ok(body)
    }

    /// `shell_exec` —— spawn 子进程 + 看门狗（idle / wall-clock 双维度），避免长时间静默或
    /// 死循环把 Coding Agent 的整体超时预算吃光。
    ///
    /// 行为：
    /// - 默认阈值：idle 60s / wall 5min（覆盖 99% 命令）
    /// - 入参 `expect_long_running: true` → 提升到 idle 120s / wall 30min（npm/cargo install 等）
    /// - 触发 idle 或 wall 超限：先 SIGTERM，2s grace 后 SIGKILL
    /// - 输出 buffer 各保留尾部 16KB（构建/测试关键信息总在尾部）
    /// - 被 watchdog 终止时返回结构化错误（含分类 + 末尾输出），让 LLM 可据此决定换种方式重试
    async fn shell_exec(
        &self,
        input: &serde_json::Value,
        stream_ctx: Option<StreamCtx>,
        _exec_ctx: Option<ToolExecutionContext>,
    ) -> ToolOutput {
        let command = match input["command"].as_str() {
            Some(c) => c,
            None => {
                return ToolOutput::error(
                    "parameter_error",
                    &format!(
                        "Missing 'command' parameter. Received fields: {}",
                        summarize_json_value_for_error(input)
                    ),
                )
            }
        };
        let expect_long_running = input
            .get("expect_long_running")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let (default_idle_secs, default_wall_secs) = if expect_long_running {
            (SHELL_LONG_IDLE_SECS, SHELL_LONG_WALL_SECS)
        } else {
            (SHELL_DEFAULT_IDLE_SECS, SHELL_DEFAULT_WALL_SECS)
        };
        let idle_secs = input
            .get("idle_timeout_seconds")
            .and_then(|v| v.as_u64())
            .unwrap_or(default_idle_secs)
            .clamp(1, SHELL_LONG_IDLE_SECS);
        let wall_secs = input
            .get("timeout_seconds")
            .or_else(|| input.get("wall_timeout_seconds"))
            .and_then(|v| v.as_u64())
            .unwrap_or(default_wall_secs)
            .clamp(1, SHELL_LONG_WALL_SECS);

        // 启动元信息：让前端 Workspace 流上立即出现一条命令开始的 marker
        if let Some(ctx) = &stream_ctx {
            emit_stream_meta(ctx, &format!("$ {command}\n"));
        }

        let cmd_excerpt: String = command.chars().take(120).collect();
        tracing::info!(
            tool = "shell_exec",
            command = %cmd_excerpt,
            command_len = command.len(),
            expect_long_running,
            idle_secs,
            wall_secs,
            workspace = %self.workspace_root.display(),
            "shell_exec spawning"
        );

        let mut command_builder = Command::new("sh");
        command_builder
            .args(["-c", command])
            .current_dir(&self.workspace_root)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        unsafe {
            command_builder.pre_exec(|| {
                if libc::setpgid(0, 0) == 0 {
                    Ok(())
                } else {
                    Err(std::io::Error::last_os_error())
                }
            });
        }

        let mut child = match command_builder.spawn() {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    tool = "shell_exec",
                    command = %cmd_excerpt,
                    error = %e,
                    "shell_exec spawn failed"
                );
                if let Some(ctx) = &stream_ctx {
                    emit_stream_meta(ctx, &format!("[spawn error] {e}\n"));
                }
                return Self::format_shell_spawn_error(e, wall_secs);
            }
        };
        let pid = child.id();
        tracing::info!(
            tool = "shell_exec",
            command = %cmd_excerpt,
            pid = ?pid,
            "shell_exec child spawned"
        );

        let stdout_pipe = child.stdout.take();
        let stderr_pipe = child.stderr.take();

        let evidence = _exec_ctx.as_ref().and_then(|ctx| {
            ShellEvidence::create(&self.workspace_root, &self.evidence_root, ctx).ok()
        });
        let stdout_evidence = evidence.as_ref().map(|e| e.stdout.clone());
        let stderr_evidence = evidence.as_ref().map(|e| e.stderr.clone());

        let last_byte_at = Arc::new(Mutex::new(Instant::now()));
        let stdout_buf = Arc::new(Mutex::new(TruncatedBuffer::new(SHELL_OUTPUT_MAX_BYTES)));
        let stderr_buf = Arc::new(Mutex::new(TruncatedBuffer::new(SHELL_OUTPUT_MAX_BYTES)));

        let stdout_handle = stdout_pipe.map(|p| {
            spawn_pipe_reader(
                p,
                stdout_buf.clone(),
                last_byte_at.clone(),
                stream_ctx.clone().map(|c| (c, "stdout".to_string())),
                stdout_evidence,
            )
        });
        let stderr_handle = stderr_pipe.map(|p| {
            spawn_pipe_reader(
                p,
                stderr_buf.clone(),
                last_byte_at.clone(),
                stream_ctx.clone().map(|c| (c, "stderr".to_string())),
                stderr_evidence,
            )
        });

        let started = Instant::now();
        let mut termination_reason: Option<String> = None;

        // 把可选的 cancel_token 解为一个永远等待 / 真实等待的 future——避免在 select!
        // 分支里用 Option 引发 borrow / Pin 麻烦。None 时直接 future::pending() 永远不就绪。
        let cancel_fut = async {
            if let Some(t) = &self.cancel_token {
                t.cancelled().await;
            } else {
                std::future::pending::<()>().await;
            }
        };
        tokio::pin!(cancel_fut);

        // 把"子进程退出"语义抽成一个 helper，被 wait() / try_wait() 兜底两条路径共用。
        // wait() 走 SIGCHLD path（fast，几乎无延迟）；try_wait() 走 polling path（兜底 race）。
        let finalize = |status: std::io::Result<std::process::ExitStatus>,
                        termination_reason: Option<String>,
                        stdout_text: String,
                        stderr_text: String,
                        elapsed: std::time::Duration| {
            Self::finalize_shell_output(
                status,
                termination_reason,
                stdout_text,
                stderr_text,
                elapsed,
                evidence.as_ref(),
                command,
            )
        };

        loop {
            tokio::select! {
                biased;
                wait_res = child.wait() => {
                    if let Some(h) = stdout_handle { let _ = h.await; }
                    if let Some(h) = stderr_handle { let _ = h.await; }

                    let stdout_text = stdout_buf.lock().unwrap().render();
                    let stderr_text = stderr_buf.lock().unwrap().render();
                    let elapsed = started.elapsed();
                    tracing::info!(
                        tool = "shell_exec",
                        command = %cmd_excerpt,
                        pid = ?pid,
                        elapsed_ms = elapsed.as_millis() as u64,
                        exit = ?wait_res.as_ref().map(|s| s.code()).ok().flatten(),
                        stdout_bytes = stdout_text.len(),
                        stderr_bytes = stderr_text.len(),
                        terminated_reason = termination_reason.as_deref().unwrap_or(""),
                        path = "wait()",
                        "shell_exec finalized"
                    );

                    return finalize(wait_res, termination_reason, stdout_text, stderr_text, elapsed);
                }
                // 用户主动取消：第一次进来打 SIGTERM，让 wait() 自然完成；
                // pin 的 future 已经 ready 后就不再 select 中（select 不会重复 poll），
                // 后续 watchdog tick 会兜底升级到 SIGKILL（terminate_child 含 grace 逻辑）。
                _ = &mut cancel_fut, if termination_reason.is_none() => {
                    termination_reason = Some("cancelled by user".to_string());
                    tracing::info!("shell_exec cancelled by user, terminating child");
                    if let Some(ctx) = &stream_ctx {
                        emit_stream_meta(ctx, "[cancelled by user]\n");
                    }
                    terminate_child(&mut child).await;
                }
                _ = tokio::time::sleep(SHELL_WATCHDOG_TICK) => {
                    // ① 兜底退出检测：multi-thread runtime 上 wait()/SIGCHLD 偶尔会错过
                    //    （tokio-rs/tokio#3520），尤其是 mkdir / cp / chmod 这种瞬时命令——
                    //    子进程几乎一上来就 exit，SIGCHLD 又快又一次性，handler 没就位就丢了。
                    //    每 tick try_wait() 一次能稳稳兜住。
                    if termination_reason.is_none() {
                        match child.try_wait() {
                            Ok(Some(status)) => {
                                if let Some(h) = stdout_handle { let _ = h.await; }
                                if let Some(h) = stderr_handle { let _ = h.await; }
                                let stdout_text = stdout_buf.lock().unwrap().render();
                                let stderr_text = stderr_buf.lock().unwrap().render();
                                let elapsed = started.elapsed();
                                tracing::info!(
                                    tool = "shell_exec",
                                    command = %cmd_excerpt,
                                    pid = ?pid,
                                    elapsed_ms = elapsed.as_millis() as u64,
                                    exit = ?status.code(),
                                    stdout_bytes = stdout_text.len(),
                                    stderr_bytes = stderr_text.len(),
                                    path = "try_wait()",
                                    "shell_exec finalized via polling fallback"
                                );
                                return finalize(
                                    Ok(status),
                                    termination_reason,
                                    stdout_text,
                                    stderr_text,
                                    elapsed,
                                );
                            }
                            Ok(None) => {} // 还在跑，继续 watchdog 检查
                            Err(e) => {
                                tracing::warn!("shell_exec try_wait error: {e}");
                                // 不终止 —— 让 wait() 继续兜，避免 try_wait 一次性 EINTR 把命令搞挂
                            }
                        }
                    }

                    if termination_reason.is_some() {
                        // 已经发了 SIGTERM 等子进程退出，循环回头继续等
                        continue;
                    }
                    let elapsed = started.elapsed();
                    let idle = last_byte_at.lock().unwrap().elapsed();

                    if elapsed.as_secs() >= wall_secs {
                        termination_reason = Some(format!(
                            "wall_clock {wall_secs}s exceeded (elapsed {:.1}s)",
                            elapsed.as_secs_f64()
                        ));
                    } else if idle.as_secs() >= idle_secs {
                        termination_reason = Some(format!(
                            "idle {idle_secs}s (last output {:.1}s ago, total elapsed {:.1}s)",
                            idle.as_secs_f64(),
                            elapsed.as_secs_f64()
                        ));
                    }

                    if let Some(reason) = &termination_reason {
                        tracing::warn!("shell_exec watchdog terminating: {reason}");
                        if let Some(ctx) = &stream_ctx {
                            emit_stream_meta(ctx, &format!("[watchdog kill] {reason}\n"));
                        }
                        terminate_child(&mut child).await;
                    }
                }
            }
        }
    }

    /// 把"被 watchdog 强制终止"的结果封装成 LLM 友好的结构化错误：
    /// content 是 JSON，error="shell_killed"，附 reason / partial_exit_code / 末尾 stdout / 末尾 stderr，
    /// 让 LLM 据此决定是换种方式（加超时声明、换命令、跳过装包）还是放弃。
    fn finalize_shell_output(
        status: std::io::Result<std::process::ExitStatus>,
        termination_reason: Option<String>,
        stdout_text: String,
        stderr_text: String,
        elapsed: std::time::Duration,
        evidence: Option<&ShellEvidence>,
        command: &str,
    ) -> ToolOutput {
        let output = match status {
            Ok(status) if termination_reason.is_some() => Self::format_killed(
                termination_reason.unwrap(),
                status.code(),
                stdout_text,
                stderr_text,
                elapsed,
            ),
            Ok(status) if status.success() => {
                let mut combined = stdout_text;
                if !stderr_text.is_empty() {
                    if !combined.is_empty() {
                        combined.push('\n');
                    }
                    combined.push_str("[stderr] ");
                    combined.push_str(&stderr_text);
                }
                ToolOutput::ok(combined)
            }
            Ok(status) => {
                let code = status.code().unwrap_or(-1);
                Self::format_shell_error(code, stdout_text, stderr_text, elapsed)
            }
            Err(e) => Self::format_shell_wait_error(e, elapsed),
        };
        Self::attach_shell_evidence(output, evidence, command, elapsed)
    }

    fn attach_shell_evidence(
        mut output: ToolOutput,
        evidence: Option<&ShellEvidence>,
        command: &str,
        elapsed: std::time::Duration,
    ) -> ToolOutput {
        let Some(evidence) = evidence else {
            return output;
        };
        let stdout_stats = evidence.stdout.lock().unwrap().stats();
        let stderr_stats = evidence.stderr.lock().unwrap().stats();
        let compacted = output.content.chars().count() > SHELL_COMPACT_THRESHOLD_CHARS
            || stdout_stats.bytes + stderr_stats.bytes > SHELL_COMPACT_THRESHOLD_CHARS;
        let proxy_env_present = std::env::var_os("ALL_PROXY").is_some()
            || std::env::var_os("all_proxy").is_some()
            || std::env::var_os("HTTPS_PROXY").is_some()
            || std::env::var_os("https_proxy").is_some()
            || std::env::var_os("HTTP_PROXY").is_some()
            || std::env::var_os("http_proxy").is_some();
        let output_kind = classify_shell_output(evidence);
        let command_family = classify_shell_command_family(command);
        let content_source = command_content_source(command);
        let content_shape =
            classify_shell_content_shape(output_kind, evidence, content_source.as_deref());
        let evidence_meta = serde_json::json!({
            "kind": "shell_exec_output",
            "output_kind": output_kind.as_str(),
            "command_family": command_family,
            "content_source": content_source.unwrap_or_else(|| "unknown".to_string()),
            "content_shape": content_shape,
            "stdout_path": evidence.stdout_path,
            "stderr_path": evidence.stderr_path,
            "stdout_bytes": stdout_stats.bytes,
            "stdout_lines": stdout_stats.lines,
            "stdout_capture_truncated": stdout_stats.capture_truncated,
            "stderr_bytes": stderr_stats.bytes,
            "stderr_lines": stderr_stats.lines,
            "stderr_capture_truncated": stderr_stats.capture_truncated,
            "compacted": compacted,
            "elapsed_seconds": elapsed.as_secs_f64(),
            "proxy_env_present": proxy_env_present,
        });
        if compacted {
            output.content = render_shell_compact_manifest(
                command,
                output.is_error,
                elapsed,
                evidence,
                stdout_stats,
                stderr_stats,
                output_kind,
            );
        }
        output.meta = Some(merge_tool_meta(output.meta.take(), evidence_meta));
        output
    }

    fn shell_error_class(exit_code: i32, stderr_text: &str) -> &'static str {
        let lower = stderr_text.to_ascii_lowercase();
        if exit_code == 127 {
            "command_not_found"
        } else if exit_code == 126 {
            "not_executable_or_permission_denied"
        } else if lower.contains("illegal option")
            || lower.contains("invalid option")
            || lower.contains("unknown option")
            || lower.contains("unsupported option")
            || lower.contains("unrecognized option")
        {
            "tool_option_or_platform_mismatch"
        } else if lower.contains("permission denied") {
            "permission_denied"
        } else {
            "nonzero_exit"
        }
    }

    fn format_shell_error(
        exit_code: i32,
        stdout_text: String,
        stderr_text: String,
        elapsed: std::time::Duration,
    ) -> ToolOutput {
        let shell_error_class = Self::shell_error_class(exit_code, &stderr_text);
        let payload = serde_json::json!({
            "error": "shell_error",
            "shell_error_class": shell_error_class,
            "capability_feedback": matches!(shell_error_class, "command_not_found" | "not_executable_or_permission_denied" | "tool_option_or_platform_mismatch" | "permission_denied"),
            "exit_code": exit_code,
            "elapsed_seconds": elapsed.as_secs_f64(),
            "stdout_tail": stdout_text,
            "stderr_tail": stderr_text,
            "hint": "Use the exit code and stderr as observed environment facts. Adapt the command to the available shell/tools instead of repeating the same failing command."
        });
        ToolOutput {
            content: payload.to_string(),
            is_error: true,
            meta: Some(serde_json::json!({
                "shell_error_class": shell_error_class,
                "capability_feedback": true,
                "exit_code": exit_code,
            })),
        }
    }

    fn format_shell_wait_error(e: std::io::Error, elapsed: std::time::Duration) -> ToolOutput {
        let payload = serde_json::json!({
            "error": "shell_error",
            "shell_error_class": "process_wait_error",
            "capability_feedback": false,
            "elapsed_seconds": elapsed.as_secs_f64(),
            "message": e.to_string(),
            "hint": "The shell process could not be awaited cleanly; inspect the error and retry only if the command itself is still appropriate."
        });
        ToolOutput {
            content: payload.to_string(),
            is_error: true,
            meta: Some(serde_json::json!({
                "shell_error_class": "process_wait_error",
                "capability_feedback": false,
            })),
        }
    }

    fn format_shell_spawn_error(e: std::io::Error, wall_secs: u64) -> ToolOutput {
        let payload = serde_json::json!({
            "error": "shell_error",
            "shell_error_class": "spawn_failure",
            "capability_feedback": true,
            "message": e.to_string(),
            "timeout_seconds": wall_secs,
            "hint": "The shell command could not be spawned in this runtime environment. Check workspace access and available shell/tool capabilities before retrying."
        });
        ToolOutput {
            content: payload.to_string(),
            is_error: true,
            meta: Some(serde_json::json!({
                "shell_error_class": "spawn_failure",
                "capability_feedback": true,
            })),
        }
    }

    fn format_killed(
        reason: String,
        partial_exit_code: Option<i32>,
        stdout_text: String,
        stderr_text: String,
        elapsed: std::time::Duration,
    ) -> ToolOutput {
        let payload = serde_json::json!({
            "error": "shell_killed",
            "reason": reason,
            "partial_exit_code": partial_exit_code,
            "elapsed_seconds": elapsed.as_secs_f64(),
            "stdout_tail": stdout_text,
            "stderr_tail": stderr_text,
            "hint": "Last command was terminated by the watchdog. If you really need a long-running command, retry with `expect_long_running: true` (idle 120s / wall 1800s). If it was an infinite loop or hung process, choose a different approach instead of retrying as-is."
        });
        ToolOutput {
            content: payload.to_string(),
            is_error: true,
            meta: None,
        }
    }

    async fn list_files(&self, input: &serde_json::Value) -> ToolOutput {
        let rel_path = input["path"].as_str().unwrap_or(".");
        let limit = input
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize)
            .unwrap_or(LIST_FILES_DEFAULT_LIMIT)
            .min(LIST_FILES_HARD_LIMIT);
        let full_path = match self.resolve_path(rel_path) {
            Ok(p) => p,
            Err(e) => return ToolOutput::error("sandbox_violation", &e.to_string()),
        };
        let base_scope = self.path_scope(&full_path);

        let mut dir = match tokio::fs::read_dir(&full_path).await {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return ToolOutput::error(
                    "file_not_found",
                    &format!("Directory not found: {rel_path}"),
                );
            }
            Err(e) => return ToolOutput::error("io_error", &e.to_string()),
        };

        let mut entries = Vec::new();
        loop {
            match dir.next_entry().await {
                Ok(Some(entry)) => {
                    let file_type = match entry.file_type().await {
                        Ok(ft) => ft,
                        Err(_) => continue,
                    };
                    let name = entry.file_name().to_string_lossy().to_string();
                    let entry_path = entry.path();
                    let entry_path = entry_path
                        .canonicalize()
                        .unwrap_or_else(|_| Self::normalize_lexical(&entry_path));
                    if !matches!(base_scope, PathScope::Evidence)
                        && is_internal_evidence_or_persisted_path(&entry_path, &self.workspace_root)
                    {
                        continue;
                    }
                    let suffix = if file_type.is_dir() { "/" } else { "" };
                    entries.push(format!("{name}{suffix}"));
                }
                Ok(None) => break,
                Err(e) => return ToolOutput::error("io_error", &e.to_string()),
            }
        }
        entries.sort();
        let total = entries.len();
        let truncated = total > limit;
        entries.truncate(limit);
        let mut out = format!(
            "[list_files path={rel_path} entries_shown={} entries_total={}{}]\n",
            entries.len(),
            total,
            if truncated { " truncated=true" } else { "" }
        );
        out.push_str(&entries.join("\n"));
        if truncated {
            out.push_str(&format!(
                "\n... [truncated {} entries; pass a more specific path or increase limit up to {LIST_FILES_HARD_LIMIT}]",
                total - limit
            ));
        }
        ToolOutput::ok(out)
    }

    async fn notebook_edit(&self, input: &serde_json::Value) -> ToolOutput {
        let rel_path = match input["path"].as_str() {
            Some(p) => p,
            None => return ToolOutput::error("parameter_error", "Missing 'path' parameter."),
        };
        let operation = match input["operation"].as_str() {
            Some(op) => op,
            None => return ToolOutput::error("parameter_error", "Missing 'operation' parameter."),
        };
        if !rel_path.ends_with(".ipynb") {
            return ToolOutput::error(
                "parameter_error",
                "notebook_edit only supports .ipynb files.",
            );
        }

        let full_path = match self.resolve_path(rel_path) {
            Ok(p) => p,
            Err(e) => return ToolOutput::error("sandbox_violation", &e.to_string()),
        };
        let canonical = full_path
            .canonicalize()
            .unwrap_or_else(|_| full_path.clone());

        let raw = match tokio::fs::read_to_string(&full_path).await {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return ToolOutput::error("file_not_found", &format!("File not found: {rel_path}"));
            }
            Err(e) => return ToolOutput::error("io_error", &e.to_string()),
        };
        let mut notebook: serde_json::Value = match serde_json::from_str(&raw) {
            Ok(v) => v,
            Err(e) => return ToolOutput::error("invalid_notebook", &e.to_string()),
        };
        if !notebook.get("cells").is_some_and(|v| v.is_array()) {
            return ToolOutput::error("invalid_notebook", "Notebook JSON is missing cells array.");
        }

        match operation {
            "read_cells" => {
                let limit = input
                    .get("limit")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(NOTEBOOK_READ_CELLS_DEFAULT_LIMIT as u64)
                    .min(NOTEBOOK_READ_CELLS_HARD_LIMIT as u64)
                    as usize;
                let cells = notebook["cells"].as_array().unwrap();
                let mut source_budget = NOTEBOOK_READ_CELLS_TOTAL_SOURCE_CHARS;
                let mut truncated_sources = 0usize;
                let rendered = cells
                    .iter()
                    .take(limit)
                    .enumerate()
                    .map(|(index, cell)| {
                        let source = notebook_source_to_string(cell.get("source"));
                        let original_chars = source.chars().count();
                        let per_cell_cap = NOTEBOOK_CELL_SOURCE_MAX_CHARS.min(source_budget);
                        let source_truncated = original_chars > per_cell_cap;
                        let rendered_source = truncate_chars(&source, per_cell_cap);
                        let rendered_chars = rendered_source.chars().count();
                        if source_truncated {
                            truncated_sources += 1;
                        }
                        source_budget = source_budget.saturating_sub(rendered_chars);
                        serde_json::json!({
                            "index": index,
                            "cell_type": cell.get("cell_type").and_then(|v| v.as_str()).unwrap_or("unknown"),
                            "source_chars": original_chars,
                            "source_truncated": source_truncated,
                            "source": rendered_source,
                        })
                    })
                    .collect::<Vec<_>>();
                ToolOutput::ok(
                    serde_json::json!({
                        "path": rel_path,
                        "cell_count": cells.len(),
                        "returned": rendered.len(),
                        "truncated_cells": cells.len().saturating_sub(rendered.len()),
                        "truncated_sources": truncated_sources,
                        "hint": if cells.len() > rendered.len() || truncated_sources > 0 {
                            "Use a smaller limit or read/update a specific cell by index. Large cell sources are truncated in read_cells output."
                        } else {
                            ""
                        },
                        "cells": rendered,
                    })
                    .to_string(),
                )
            }
            "insert_cell" => {
                let source = match parse_notebook_source(input.get("source")) {
                    Ok(s) => s,
                    Err(e) => return ToolOutput::error("parameter_error", &e),
                };
                let cell_type = input["cell_type"].as_str().unwrap_or("code");
                let insert_index = match notebook_insert_index(&notebook, input) {
                    Ok(i) => i,
                    Err(e) => return ToolOutput::error("parameter_error", &e),
                };
                let cell_count = {
                    let cells = notebook["cells"].as_array_mut().unwrap();
                    if insert_index > cells.len() {
                        return ToolOutput::error(
                            "parameter_error",
                            &format!(
                                "insert index {insert_index} is past cell count {}",
                                cells.len()
                            ),
                        );
                    }
                    cells.insert(insert_index, make_notebook_cell(cell_type, source));
                    cells.len()
                };
                if let Err(e) = write_notebook_json(&full_path, &notebook).await {
                    return ToolOutput::error("io_error", &e.to_string());
                }
                self.record_path_mtime(canonical);
                ToolOutput::ok(
                    serde_json::json!({
                        "path": rel_path,
                        "operation": operation,
                        "inserted_index": insert_index,
                        "cell_count": cell_count,
                    })
                    .to_string(),
                )
            }
            "update_cell" => {
                let index = match input.get("index").and_then(|v| v.as_u64()) {
                    Some(i) => i as usize,
                    None => {
                        return ToolOutput::error("parameter_error", "update_cell requires index.")
                    }
                };
                let source = match parse_notebook_source(input.get("source")) {
                    Ok(s) => s,
                    Err(e) => return ToolOutput::error("parameter_error", &e),
                };
                let cell_count = {
                    let cells = notebook["cells"].as_array_mut().unwrap();
                    if index >= cells.len() {
                        return ToolOutput::error(
                            "parameter_error",
                            &format!(
                                "cell index {index} is out of range for {} cells",
                                cells.len()
                            ),
                        );
                    }
                    if let Some(cell_type) = input["cell_type"].as_str() {
                        cells[index]["cell_type"] =
                            serde_json::Value::String(cell_type.to_string());
                    }
                    cells[index]["source"] = source_lines_json(&source);
                    if cells[index].get("metadata").is_none() {
                        cells[index]["metadata"] = serde_json::json!({});
                    }
                    cells.len()
                };
                if let Err(e) = write_notebook_json(&full_path, &notebook).await {
                    return ToolOutput::error("io_error", &e.to_string());
                }
                self.record_path_mtime(canonical);
                ToolOutput::ok(
                    serde_json::json!({
                        "path": rel_path,
                        "operation": operation,
                        "updated_index": index,
                        "cell_count": cell_count,
                    })
                    .to_string(),
                )
            }
            "delete_cell" => {
                let index = match input.get("index").and_then(|v| v.as_u64()) {
                    Some(i) => i as usize,
                    None => {
                        return ToolOutput::error("parameter_error", "delete_cell requires index.")
                    }
                };
                let cell_count = {
                    let cells = notebook["cells"].as_array_mut().unwrap();
                    if index >= cells.len() {
                        return ToolOutput::error(
                            "parameter_error",
                            &format!(
                                "cell index {index} is out of range for {} cells",
                                cells.len()
                            ),
                        );
                    }
                    cells.remove(index);
                    cells.len()
                };
                if let Err(e) = write_notebook_json(&full_path, &notebook).await {
                    return ToolOutput::error("io_error", &e.to_string());
                }
                self.record_path_mtime(canonical);
                ToolOutput::ok(
                    serde_json::json!({
                        "path": rel_path,
                        "operation": operation,
                        "deleted_index": index,
                        "cell_count": cell_count,
                    })
                    .to_string(),
                )
            }
            _ => ToolOutput::error(
                "parameter_error",
                "operation must be one of read_cells, insert_cell, update_cell, delete_cell.",
            ),
        }
    }

    fn resolve_path(&self, rel_path: &str) -> Result<PathBuf> {
        let candidate = PathBuf::from(rel_path);
        let full = if candidate.is_absolute() {
            candidate
        } else {
            self.workspace_root.join(rel_path)
        };
        let canonical = full
            .canonicalize()
            .unwrap_or_else(|_| Self::normalize_lexical(&full));

        let workspace_canonical = self
            .workspace_root
            .canonicalize()
            .unwrap_or_else(|_| Self::normalize_lexical(&self.workspace_root));
        let evidence_canonical = self
            .evidence_root
            .canonicalize()
            .unwrap_or_else(|_| Self::normalize_lexical(&self.evidence_root));

        if !canonical.starts_with(&workspace_canonical)
            && !canonical.starts_with(&evidence_canonical)
        {
            bail!(
                "Path escapes workspace: {} is outside {} or evidence root {}",
                canonical.display(),
                workspace_canonical.display(),
                evidence_canonical.display()
            );
        }
        Ok(full)
    }

    fn path_scope(&self, path: &std::path::Path) -> PathScope {
        let canonical = path
            .canonicalize()
            .unwrap_or_else(|_| Self::normalize_lexical(path));
        let workspace = self
            .workspace_root
            .canonicalize()
            .unwrap_or_else(|_| Self::normalize_lexical(&self.workspace_root));
        let evidence = self
            .evidence_root
            .canonicalize()
            .unwrap_or_else(|_| Self::normalize_lexical(&self.evidence_root));
        if canonical.starts_with(&evidence) {
            PathScope::Evidence
        } else if canonical.starts_with(&workspace) {
            PathScope::Workspace
        } else {
            PathScope::Other
        }
    }

    fn display_path(&self, path: &std::path::Path) -> String {
        path_display_for_agent(&self.workspace_root, &self.evidence_root, path)
    }

    /// Resolve `.` and `..` components lexically (without touching the filesystem).
    fn normalize_lexical(path: &std::path::Path) -> PathBuf {
        use std::path::Component;
        let mut parts = Vec::new();
        for component in path.components() {
            match component {
                Component::ParentDir => {
                    if !parts.is_empty() {
                        parts.pop();
                    }
                }
                Component::CurDir => {}
                c => parts.push(c),
            }
        }
        parts.iter().collect()
    }
}

fn is_internal_evidence_or_persisted_path(
    path: &std::path::Path,
    workspace_root: &std::path::Path,
) -> bool {
    let normalized = ToolExecutor::normalize_lexical(path);
    let workspace = workspace_root
        .canonicalize()
        .unwrap_or_else(|_| ToolExecutor::normalize_lexical(workspace_root));
    if !normalized.starts_with(&workspace) {
        return false;
    }
    if normalized.components().any(|component| {
        let name = component.as_os_str().to_string_lossy();
        name == ".miragenty-evidence" || name == "tool-results"
    }) {
        return true;
    }
    is_mirrored_benchmark_asset_path(&normalized, &workspace)
}

fn is_mirrored_benchmark_asset_path(
    path: &std::path::Path,
    workspace_root: &std::path::Path,
) -> bool {
    let workspace_name = workspace_root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if workspace_name.is_empty() {
        return false;
    }
    let Ok(rel) = path.strip_prefix(workspace_root) else {
        return false;
    };
    if rel.components().count() == 1 && rel == std::path::Path::new("assets") {
        return true;
    }
    let mut components = rel.components();
    matches!(
        (components.next(), components.next()),
        (Some(std::path::Component::Normal(first)), Some(std::path::Component::Normal(second)))
            if first == "assets" && second == workspace_name
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PathScope {
    Workspace,
    Evidence,
    Other,
}

fn notebook_source_to_string(source: Option<&serde_json::Value>) -> String {
    match source {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(lines)) => lines
            .iter()
            .filter_map(|line| line.as_str())
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

fn parse_notebook_source(
    source: Option<&serde_json::Value>,
) -> std::result::Result<String, String> {
    match source {
        Some(serde_json::Value::String(s)) => Ok(s.clone()),
        Some(serde_json::Value::Array(lines)) => {
            let mut out = String::new();
            for (i, line) in lines.iter().enumerate() {
                let Some(line) = line.as_str() else {
                    return Err(format!("source[{i}] must be a string"));
                };
                out.push_str(line);
            }
            Ok(out)
        }
        Some(_) => Err("source must be a string or array of strings".to_string()),
        None => Err("Missing 'source' parameter.".to_string()),
    }
}

fn source_lines_json(source: &str) -> serde_json::Value {
    if source.is_empty() {
        return serde_json::json!([]);
    }
    let mut lines = source
        .split_inclusive('\n')
        .map(|line| serde_json::Value::String(line.to_string()))
        .collect::<Vec<_>>();
    if !source.ends_with('\n') {
        if lines.is_empty() {
            lines.push(serde_json::Value::String(source.to_string()));
        }
    }
    serde_json::Value::Array(lines)
}

fn make_notebook_cell(cell_type: &str, source: String) -> serde_json::Value {
    let mut cell = serde_json::json!({
        "cell_type": cell_type,
        "metadata": {},
        "source": source_lines_json(&source),
    });
    if cell_type == "code" {
        cell["execution_count"] = serde_json::Value::Null;
        cell["outputs"] = serde_json::json!([]);
    }
    cell
}

fn notebook_insert_index(
    notebook: &serde_json::Value,
    input: &serde_json::Value,
) -> std::result::Result<usize, String> {
    let cells = notebook["cells"]
        .as_array()
        .ok_or_else(|| "Notebook JSON is missing cells array.".to_string())?;
    if let Some(index) = input.get("index").and_then(|v| v.as_u64()) {
        return Ok(index as usize);
    }
    if let Some(index) = input.get("after_cell_index").and_then(|v| v.as_u64()) {
        let index = index as usize;
        if index >= cells.len() {
            return Err(format!(
                "after_cell_index {index} is out of range for {} cells",
                cells.len()
            ));
        }
        return Ok(index + 1);
    }
    if let Some(needle) = input.get("after_source_contains").and_then(|v| v.as_str()) {
        let Some((index, _)) = cells
            .iter()
            .enumerate()
            .find(|(_, cell)| notebook_source_to_string(cell.get("source")).contains(needle))
        else {
            return Err(format!("no cell source contains {needle:?}"));
        };
        return Ok(index + 1);
    }
    Ok(cells.len())
}

async fn write_notebook_json(path: &std::path::Path, notebook: &serde_json::Value) -> Result<()> {
    let content = serde_json::to_string_pretty(notebook)?;
    tokio::fs::write(path, format!("{content}\n")).await?;
    Ok(())
}

struct EditError {
    kind: String,
    message: String,
}

/// Single-Agent Uplift B3 + B4: 在内存里应用一条 (old_string -> new_string) 替换。
///
/// 命中策略（按顺序，命中即用）：
///   1. 原样匹配
///   2. strip 行号前缀（`  42|...` → `...`），匹配后用 stripped old_string 在原文里做替换
///   3. 双方做 desanitize 标准化（curly quotes / NBSP / 全角空格 → ASCII），匹配后
///      在原文里做"标准化窗口替换"——即：找到原文里的标准化匹配位置，按原文实际字节替换为
///      desanitized new_string。
///
/// 返回 `(updated_content, replacements, fallback_used_label)`。`fallback_used_label`
/// 在原样命中时为 `"exact"`，命中行号 strip 时为 `"stripped_line_numbers"`，命中
/// desanitize 时为 `"desanitized"`。
fn apply_single_edit(
    content: &str,
    old_string: &str,
    new_string: &str,
    replace_all: bool,
) -> Result<(String, u64, &'static str), EditError> {
    // 1. 原样匹配
    let count = content.matches(old_string).count();
    if count == 1 || (count >= 1 && replace_all) {
        let updated = if replace_all {
            content.replace(old_string, new_string)
        } else {
            content.replacen(old_string, new_string, 1)
        };
        return Ok((updated, count as u64, "exact"));
    }
    if count > 1 {
        return Err(EditError {
            kind: "edit_not_unique".into(),
            message: format!(
                "old_string is not unique: matched {count} times. Either expand old_string \
                 to include more surrounding context (2-4 adjacent lines is usually enough) \
                 or pass replace_all=true if you really want to change every occurrence."
            ),
        });
    }

    // 2. strip 行号前缀 fallback
    if let Some(stripped_old) = strip_line_number_prefix(old_string) {
        let stripped_new =
            strip_line_number_prefix(new_string).unwrap_or_else(|| new_string.to_string());
        let count = content.matches(&stripped_old).count();
        if count == 1 || (count >= 1 && replace_all) {
            let updated = if replace_all {
                content.replace(&stripped_old, &stripped_new)
            } else {
                content.replacen(&stripped_old, &stripped_new, 1)
            };
            return Ok((updated, count as u64, "stripped_line_numbers"));
        }
        if count > 1 {
            return Err(EditError {
                kind: "edit_not_unique".into(),
                message: format!(
                    "old_string (with line-number prefix stripped) is not unique: matched \
                     {count} times. Expand old_string with more context, or pass replace_all=true."
                ),
            });
        }
    }

    // 3. desanitize fallback：把 curly/NBSP 等"看不见的等价字符"标准化后比对。
    let san_old = desanitize_text(old_string);
    let san_new = desanitize_text(new_string);
    let san_content = desanitize_text(content);
    let count = san_content.matches(&san_old).count();
    if count == 1 || (count >= 1 && replace_all) {
        let updated = if replace_all {
            san_content.replace(&san_old, &san_new)
        } else {
            san_content.replacen(&san_old, &san_new, 1)
        };
        // 注意：返回的是 desanitized 版本——原文里的 curly quote 会被改写为 ASCII。
        // 这是有意的：LLM 想要的明显是 ASCII 语义，curly 是工具链途中污染。
        return Ok((updated, count as u64, "desanitized"));
    }

    Err(EditError {
        kind: "edit_no_match".into(),
        message:
            "old_string not found in the file (tried exact match, line-number-prefix strip, and \
             curly-quote/whitespace normalization). Did the file change since you last read it? \
             Re-run read_file and copy old_string verbatim from the output."
                .into(),
    })
}

/// 检测并移除 `read_file` 输出风格的行号前缀：
/// 每行以最多 6 个空格 + 数字 + `|` 开头。**只有当所有非空行都符合**才认定为行号块，
/// 避免误伤普通含 `|` 的代码（如 Rust 模式匹配）。
fn strip_line_number_prefix(s: &str) -> Option<String> {
    let lines: Vec<&str> = s.split('\n').collect();
    if lines.is_empty() {
        return None;
    }
    let mut all_match = true;
    let mut any_real = false;
    for line in &lines {
        if line.is_empty() {
            continue;
        }
        any_real = true;
        if !looks_like_line_number_prefix(line) {
            all_match = false;
            break;
        }
    }
    if !all_match || !any_real {
        return None;
    }
    let stripped: Vec<String> = lines
        .iter()
        .map(|line| {
            if line.is_empty() {
                String::new()
            } else {
                strip_one_line_prefix(line).unwrap_or_else(|| (*line).to_string())
            }
        })
        .collect();
    Some(stripped.join("\n"))
}

fn looks_like_line_number_prefix(line: &str) -> bool {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i] == b' ' && i < 6 {
        i += 1;
    }
    let digit_start = i;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i == digit_start {
        return false;
    }
    i < bytes.len() && bytes[i] == b'|'
}

fn strip_one_line_prefix(line: &str) -> Option<String> {
    let pipe = line.find('|')?;
    let prefix = &line[..pipe];
    if prefix.trim().chars().all(|c| c.is_ascii_digit()) && !prefix.trim().is_empty() {
        Some(line[pipe + 1..].to_string())
    } else {
        None
    }
}

/// 把"看起来像但字节不一样"的字符标准化为 ASCII。覆盖 LLM/IME/前端富文本编辑器
/// 最常见的污染：
///   - U+2018 / U+2019 (` ` ` `) → ASCII '
///   - U+201C / U+201D ( ) → ASCII "
///   - U+2013 (–) → '-'  (en dash → hyphen)
///   - U+2014 (—) → '-'  (em dash → hyphen)
///   - U+00A0 (NBSP) → ASCII ' '
///   - U+3000 (全角空格) → ASCII ' '
///   - U+FEFF (BOM/ZWNBSP) → 删除
fn desanitize_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\u{2018}' | '\u{2019}' => out.push('\''),
            '\u{201C}' | '\u{201D}' => out.push('"'),
            '\u{2013}' | '\u{2014}' => out.push('-'),
            '\u{00A0}' | '\u{3000}' => out.push(' '),
            '\u{FEFF}' => {}
            other => out.push(other),
        }
    }
    out
}

#[derive(Debug, Clone, Copy)]
struct EvidenceStats {
    bytes: usize,
    lines: usize,
    capture_truncated: bool,
}

struct EvidenceSink {
    path: PathBuf,
    file: Option<std::fs::File>,
    bytes: usize,
    lines: usize,
    capture_truncated: bool,
}

impl EvidenceSink {
    fn new(path: PathBuf) -> Self {
        let file = std::fs::File::create(&path).ok();
        Self {
            path,
            file,
            bytes: 0,
            lines: 0,
            capture_truncated: false,
        }
    }

    fn write_chunk(&mut self, bytes: &[u8]) {
        self.lines += bytes.iter().filter(|b| **b == b'\n').count();
        if self.bytes >= SHELL_EVIDENCE_CAPTURE_MAX_BYTES {
            self.capture_truncated = true;
            return;
        }
        let remaining = SHELL_EVIDENCE_CAPTURE_MAX_BYTES - self.bytes;
        let to_write = bytes.len().min(remaining);
        if to_write < bytes.len() {
            self.capture_truncated = true;
        }
        if let Some(file) = &mut self.file {
            let _ = file.write_all(&bytes[..to_write]);
        }
        self.bytes += to_write;
    }

    fn stats(&self) -> EvidenceStats {
        EvidenceStats {
            bytes: self.bytes,
            lines: self.lines,
            capture_truncated: self.capture_truncated,
        }
    }
}

struct ShellEvidence {
    stdout_path: String,
    stderr_path: String,
    stdout: Arc<Mutex<EvidenceSink>>,
    stderr: Arc<Mutex<EvidenceSink>>,
}

impl ShellEvidence {
    fn create(
        workspace_root: &std::path::Path,
        evidence_root: &std::path::Path,
        ctx: &ToolExecutionContext,
    ) -> Result<Self> {
        let dir = evidence_root
            .join(sanitize_path_segment(&ctx.agent_id))
            .join(format!("step-{:04}", ctx.step))
            .join(sanitize_path_segment(&ctx.tool_use_id));
        std::fs::create_dir_all(&dir)?;
        let stdout = dir.join("stdout.txt");
        let stderr = dir.join("stderr.txt");
        Ok(Self {
            stdout_path: path_display_for_agent(workspace_root, evidence_root, &stdout),
            stderr_path: path_display_for_agent(workspace_root, evidence_root, &stderr),
            stdout: Arc::new(Mutex::new(EvidenceSink::new(stdout))),
            stderr: Arc::new(Mutex::new(EvidenceSink::new(stderr))),
        })
    }
}

fn sanitize_path_segment(value: &str) -> String {
    let cleaned: String = value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
                c
            } else {
                '-'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "unknown".to_string()
    } else {
        cleaned
    }
}

fn path_display_for_agent(
    workspace_root: &std::path::Path,
    evidence_root: &std::path::Path,
    path: &std::path::Path,
) -> String {
    if let Ok(relative) = path.strip_prefix(workspace_root) {
        return relative.display().to_string();
    }
    if let Ok(relative) = path.strip_prefix(evidence_root) {
        return evidence_root.join(relative).display().to_string();
    }
    path.display().to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShellOutputKind {
    Log,
    Html,
    Data,
    Text,
}

impl ShellOutputKind {
    fn as_str(self) -> &'static str {
        match self {
            ShellOutputKind::Log => "log",
            ShellOutputKind::Html => "html",
            ShellOutputKind::Data => "data",
            ShellOutputKind::Text => "text",
        }
    }
}

fn classify_shell_output(evidence: &ShellEvidence) -> ShellOutputKind {
    let stdout = evidence
        .stdout
        .lock()
        .ok()
        .and_then(|sink| std::fs::read_to_string(&sink.path).ok())
        .unwrap_or_default();
    let stderr = evidence
        .stderr
        .lock()
        .ok()
        .and_then(|sink| std::fs::read_to_string(&sink.path).ok())
        .unwrap_or_default();
    let sample = format!(
        "{}\n{}",
        stdout.chars().take(16_000).collect::<String>(),
        stderr.chars().take(4_000).collect::<String>()
    );
    let lower = sample.to_ascii_lowercase();
    if lower.contains("<!doctype html") || lower.contains("<html") || lower.contains("</html>") {
        return ShellOutputKind::Html;
    }
    if stderr.lines().any(|line| is_log_signal_line(line))
        || lower.contains("traceback")
        || lower.contains("npm err!")
        || lower.contains("error[")
    {
        return ShellOutputKind::Log;
    }
    let dataish_lines = stdout
        .lines()
        .take(80)
        .filter(|line| {
            let trimmed = line.trim();
            (trimmed.starts_with('{') && trimmed.ends_with('}'))
                || (trimmed.starts_with('[') && trimmed.ends_with(']'))
                || trimmed.matches(',').count() >= 2
                || trimmed.matches('\t').count() >= 2
                || trimmed.matches('|').count() >= 2
        })
        .count();
    if dataish_lines >= 2 {
        return ShellOutputKind::Data;
    }
    ShellOutputKind::Text
}

fn is_log_signal_line(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    [
        "error:",
        "error[",
        " failed",
        "failure",
        "panic",
        "exception",
        "traceback",
        "assert",
        "not found",
        "permission denied",
        "warning:",
        "npm err!",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn classify_shell_command_family(command: &str) -> &'static str {
    let lower = command.to_ascii_lowercase();
    let first = lower.split_whitespace().next().unwrap_or("");
    if (matches!(
        first,
        "cargo" | "npm" | "pnpm" | "yarn" | "pytest" | "go" | "make"
    ) && (lower.contains(" test")
        || lower.contains(" check")
        || lower.contains(" build")
        || lower.contains(" run")))
        || lower.contains("python -m pytest")
        || lower.contains("python3 -m pytest")
    {
        return "build_test";
    }
    if matches!(first, "curl" | "wget")
        || lower.contains("requests.get")
        || lower.contains("httpx.get")
        || lower.contains("urllib.request")
    {
        return "fetch";
    }
    if matches!(first, "cat" | "jq") || lower.contains(".read()") || lower.contains("read_text(") {
        return "file_dump";
    }
    if matches!(first, "head" | "tail" | "sed" | "awk") {
        return "file_excerpt";
    }
    if matches!(first, "ls" | "find") {
        return "directory_listing";
    }
    if lower.contains("assert")
        || lower.contains("required")
        || lower.contains("json.load")
        || lower.contains("csv")
        || lower.contains("wc -")
    {
        return "validation";
    }
    "generic"
}

fn command_content_source(command: &str) -> Option<String> {
    extract_url(command).or_else(|| extract_probable_path(command))
}

fn extract_url(text: &str) -> Option<String> {
    for token in
        text.split(|c: char| c.is_whitespace() || matches!(c, '\'' | '"' | ')' | '(' | '<' | '>'))
    {
        if token.starts_with("http://") || token.starts_with("https://") {
            return Some(
                token
                    .trim_end_matches(|c: char| matches!(c, ',' | ';'))
                    .to_string(),
            );
        }
    }
    None
}

fn extract_probable_path(command: &str) -> Option<String> {
    command
        .split_whitespace()
        .map(|t| t.trim_matches(|c| matches!(c, '\'' | '"' | ',' | ';')))
        .find(|t| {
            t.contains('/')
                || [
                    ".json", ".csv", ".md", ".txt", ".html", ".xml", ".ipynb", ".rs", ".ts", ".tsx",
                ]
                .iter()
                .any(|ext| t.ends_with(ext))
        })
        .map(|s| s.to_string())
}

fn classify_shell_content_shape(
    output_kind: ShellOutputKind,
    evidence: &ShellEvidence,
    source: Option<&str>,
) -> &'static str {
    if let Some(source) = source {
        if let Some(shape) = shape_from_path_or_url(source) {
            return shape;
        }
    }
    let stdout_path = evidence.stdout.lock().unwrap().path.clone();
    let sample = std::fs::read_to_string(stdout_path).unwrap_or_default();
    classify_text_shape(&sample)
        .strip_prefix("")
        .unwrap_or(match output_kind {
            ShellOutputKind::Html => "html",
            ShellOutputKind::Data => "json",
            ShellOutputKind::Log => "log",
            ShellOutputKind::Text => "text",
        })
}

fn classify_text_shape(content: &str) -> &'static str {
    let sample = content.trim_start();
    let lower = sample
        .chars()
        .take(512)
        .collect::<String>()
        .to_ascii_lowercase();
    if lower.contains("<!doctype html") || lower.contains("<html") {
        "html"
    } else if lower.starts_with("<?xml") || lower.starts_with("<feed") || lower.starts_with("<rss")
    {
        "xml"
    } else if sample.starts_with('{') || sample.starts_with('[') {
        "json"
    } else if sample
        .lines()
        .take(5)
        .any(|line| line.matches(',').count() >= 2)
    {
        "csv"
    } else if sample.lines().take(20).any(|line| line.starts_with('#')) {
        "markdown"
    } else if lower.contains("traceback") || lower.contains("error:") || lower.contains("warning:")
    {
        "log"
    } else {
        "text"
    }
}

fn shape_from_path_or_url(source: &str) -> Option<&'static str> {
    let lower = source.to_ascii_lowercase();
    if lower.ends_with(".html") || lower.ends_with(".htm") {
        Some("html")
    } else if lower.ends_with(".json") {
        Some("json")
    } else if lower.ends_with(".xml") || lower.ends_with(".rss") {
        Some("xml")
    } else if lower.ends_with(".csv") || lower.ends_with(".tsv") {
        Some("csv")
    } else if lower.ends_with(".md") || lower.ends_with(".markdown") {
        Some("markdown")
    } else if lower.ends_with(".ipynb") {
        Some("notebook")
    } else {
        None
    }
}

fn render_evidence_read_ref(path: &str, content: &str, bytes: u64, lines: usize) -> String {
    let mut out = format!(
        "[evidence_read_ref]\npath: {path}\nbytes: {bytes}\nlines: {lines}\ncontent_shape: {}\n",
        classify_text_shape(content)
    );
    out.push_str("\nExcerpt:\n");
    out.push_str(&head_tail_excerpt(content, EVIDENCE_READ_EXCERPT_CHARS));
    out.push_str("\n\nSuggested next steps:\n");
    out.push_str(&format!(
        "- grep {{\"path\":\"{path}\",\"pattern\":\"<narrow-pattern>\",\"context\":4}}\n- read_file {{\"path\":\"{path}\",\"offset\":<line>,\"limit\":120}}"
    ));
    out
}

fn head_tail_excerpt(content: &str, max_chars: usize) -> String {
    let count = content.chars().count();
    if count <= max_chars {
        return content.to_string();
    }
    let head_len = max_chars / 2;
    let tail_len = max_chars.saturating_sub(head_len);
    let head = content.chars().take(head_len).collect::<String>();
    let tail = content
        .chars()
        .rev()
        .take(tail_len)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<String>();
    format!(
        "{head}\n... [middle omitted: {} chars] ...\n{tail}",
        count.saturating_sub(max_chars)
    )
}

fn strip_html_tags(line: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in line.chars() {
        match c {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                out.push(' ');
            }
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn html_high_signal_excerpts(evidence: &ShellEvidence) -> Vec<String> {
    let stdout_path = evidence.stdout.lock().unwrap().path.clone();
    let Ok(content) = std::fs::read_to_string(stdout_path) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (idx, raw_line) in content.lines().enumerate() {
        let line = raw_line.trim();
        let lower = line.to_ascii_lowercase();
        let is_signal = lower.contains("<title")
            || lower.contains("<h1")
            || lower.contains("<h2")
            || lower.contains("<meta name=\"description")
            || lower.contains("abstract")
            || lower.contains("paper")
            || lower.contains("model")
            || lower.contains("price")
            || lower.contains("pricing")
            || lower.contains("table");
        let is_noise = lower.contains("console.error")
            || lower.contains("--error")
            || lower.contains(".error")
            || lower.contains("error-text")
            || lower.contains("stylesheet")
            || lower.contains("script");
        if is_signal && !is_noise {
            let display = strip_html_tags(line);
            if display.is_empty() {
                continue;
            }
            out.push(format!(
                "stdout:{}: {}",
                idx + 1,
                truncate_chars(&display, 220)
            ));
            if out.len() >= 12 {
                return out;
            }
        }
    }
    out
}

fn data_high_signal_excerpts(evidence: &ShellEvidence) -> Vec<String> {
    let stdout_path = evidence.stdout.lock().unwrap().path.clone();
    let Ok(content) = std::fs::read_to_string(stdout_path) else {
        return Vec::new();
    };
    content
        .lines()
        .enumerate()
        .filter_map(|(idx, line)| {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                return None;
            }
            let dataish = (trimmed.starts_with('{') && trimmed.ends_with('}'))
                || (trimmed.starts_with('[') && trimmed.ends_with(']'))
                || trimmed.matches(',').count() >= 2
                || trimmed.matches('\t').count() >= 2
                || trimmed.matches('|').count() >= 2;
            dataish.then(|| format!("stdout:{}: {}", idx + 1, truncate_chars(trimmed, 220)))
        })
        .take(12)
        .collect()
}

fn text_high_signal_excerpts(evidence: &ShellEvidence) -> Vec<String> {
    let stdout_path = evidence.stdout.lock().unwrap().path.clone();
    let Ok(content) = std::fs::read_to_string(stdout_path) else {
        return Vec::new();
    };
    content
        .lines()
        .enumerate()
        .filter_map(|(idx, line)| {
            let trimmed = line.trim();
            if trimmed.len() < 20 {
                return None;
            }
            Some(format!(
                "stdout:{}: {}",
                idx + 1,
                truncate_chars(trimmed, 220)
            ))
        })
        .take(12)
        .collect()
}
fn render_shell_compact_manifest(
    command: &str,
    is_error: bool,
    elapsed: std::time::Duration,
    evidence: &ShellEvidence,
    stdout_stats: EvidenceStats,
    stderr_stats: EvidenceStats,
    output_kind: ShellOutputKind,
) -> String {
    let mut out = format!(
        "[shell_exec_output_compacted]\ncommand: {}\nstatus: {}\nelapsed_seconds: {:.1}\noutput_kind: {}\nstdout: {} lines, {} bytes -> {}{}\nstderr: {} lines, {} bytes -> {}{}\n",
        truncate_chars(command, 240),
        if is_error { "error" } else { "success" },
        elapsed.as_secs_f64(),
        output_kind.as_str(),
        stdout_stats.lines,
        stdout_stats.bytes,
        evidence.stdout_path,
        if stdout_stats.capture_truncated { " (capture truncated)" } else { "" },
        stderr_stats.lines,
        stderr_stats.bytes,
        evidence.stderr_path,
        if stderr_stats.capture_truncated { " (capture truncated)" } else { "" },
    );
    out.push_str("\nHigh-signal excerpts:\n");
    let excerpts = collect_high_signal_excerpts(evidence, output_kind);
    if excerpts.is_empty() {
        out.push_str("- (none detected; inspect evidence with grep/read_file if needed)\n");
    } else {
        for line in excerpts {
            out.push_str("- ");
            out.push_str(&line);
            out.push('\n');
        }
    }
    out.push_str("\nSuggested next steps:\n");
    match output_kind {
        ShellOutputKind::Log => {
            out.push_str(&format!(
                "- grep {{\"pattern\":\"error|failed|panic|Traceback|Exception|warning\",\"path\":\"{}\",\"context\":6}}\n",
                evidence.stderr_path
            ));
            out.push_str(&format!(
                "- read_file {{\"path\":\"{}\",\"offset\":1,\"limit\":120}}\n",
                evidence.stderr_path
            ));
        }
        ShellOutputKind::Html => {
            out.push_str(&format!(
                "- grep {{\"pattern\":\"<title|<h1|<h2|abstract|model|price|pricing|table\",\"path\":\"{}\",\"context\":4}}\n",
                evidence.stdout_path
            ));
            out.push_str(&format!(
                "- read_file {{\"path\":\"{}\",\"offset\":1,\"limit\":160}}\n",
                evidence.stdout_path
            ));
        }
        ShellOutputKind::Data => {
            out.push_str(&format!(
                "- read_file {{\"path\":\"{}\",\"offset\":1,\"limit\":80}}\n",
                evidence.stdout_path
            ));
            out.push_str("- Use a focused shell/python parser on the evidence file if exact aggregation is needed.\n");
        }
        ShellOutputKind::Text => {
            out.push_str(&format!(
                "- grep {{\"pattern\":\"TODO|ERROR|result|summary|conclusion|key|required\",\"path\":\"{}\",\"context\":4}}\n",
                evidence.stdout_path
            ));
            out.push_str(&format!(
                "- read_file {{\"path\":\"{}\",\"offset\":1,\"limit\":120}}\n",
                evidence.stdout_path
            ));
        }
    }
    truncate_chars(&out, SHELL_COMPACT_MAX_CHARS)
}

fn collect_high_signal_excerpts(
    evidence: &ShellEvidence,
    output_kind: ShellOutputKind,
) -> Vec<String> {
    match output_kind {
        ShellOutputKind::Html => html_high_signal_excerpts(evidence),
        ShellOutputKind::Data => data_high_signal_excerpts(evidence),
        ShellOutputKind::Text => text_high_signal_excerpts(evidence),
        ShellOutputKind::Log => collect_log_high_signal_excerpts(evidence),
    }
}

fn collect_log_high_signal_excerpts(evidence: &ShellEvidence) -> Vec<String> {
    let stderr_path = evidence.stderr.lock().unwrap().path.clone();
    let stdout_path = evidence.stdout.lock().unwrap().path.clone();
    let mut out = Vec::new();
    for (label, path) in [("stderr", stderr_path), ("stdout", stdout_path)] {
        if let Ok(content) = std::fs::read_to_string(path) {
            for (idx, line) in content.lines().enumerate() {
                if is_log_signal_line(line) {
                    out.push(format!(
                        "{label}:{}: {}",
                        idx + 1,
                        truncate_chars(line, 220)
                    ));
                    if out.len() >= 12 {
                        return out;
                    }
                }
            }
        }
    }
    out
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut truncated = s.chars().take(max.saturating_sub(1)).collect::<String>();
        truncated.push('…');
        truncated
    }
}

fn truncate_middle_chars(s: &str, max: usize) -> String {
    let count = s.chars().count();
    if count <= max {
        return s.to_string();
    }
    if max <= 1 {
        return "…".to_string();
    }
    let head_len = max / 2;
    let tail_len = max.saturating_sub(head_len + 1);
    let head: String = s.chars().take(head_len).collect();
    let tail: String = s
        .chars()
        .rev()
        .take(tail_len)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("{head}…{tail}")
}

fn cap_text_with_notice(text: String, max_chars: usize, notice: &str) -> String {
    if text.chars().count() <= max_chars {
        return text;
    }
    let kept = text.chars().take(max_chars).collect::<String>();
    format!("{kept}\n... [truncated to {max_chars} chars; {notice}]")
}

fn capability_tool_error(tool: &str, class: &str, message: String) -> ToolOutput {
    let payload = serde_json::json!({
        "error": "tool_capability_error",
        "tool": tool,
        "capability_error_class": class,
        "capability_feedback": true,
        "message": message,
        "hint": "This tool dependency is not available or could not be launched in the current runtime environment. Adapt using the runtime profile and available tools instead of retrying the same failing call."
    });
    ToolOutput {
        content: payload.to_string(),
        is_error: true,
        meta: Some(serde_json::json!({
            "capability_feedback": true,
            "capability_error_class": class,
            "tool": tool,
        })),
    }
}

fn content_source_from_tool_context(tool_name: &str, output: &ToolOutput) -> String {
    output
        .meta
        .as_ref()
        .and_then(|meta| meta.get("content_source"))
        .and_then(|value| value.as_str())
        .map(|value| value.to_string())
        .or_else(|| {
            output
                .meta
                .as_ref()
                .and_then(|meta| meta.get("path"))
                .and_then(|value| value.as_str())
                .map(|value| value.to_string())
        })
        .unwrap_or_else(|| tool_name.to_string())
}

fn merge_tool_meta(
    existing: Option<serde_json::Value>,
    next: serde_json::Value,
) -> serde_json::Value {
    match (existing, next) {
        (Some(serde_json::Value::Object(mut base)), serde_json::Value::Object(add)) => {
            for (k, v) in add {
                base.insert(k, v);
            }
            serde_json::Value::Object(base)
        }
        (Some(existing), next) => serde_json::json!({
            "previous_meta": existing,
            "meta": next,
        }),
        (None, next) => next,
    }
}

fn summarize_json_value_for_error(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Object(map) => {
            let fields = map
                .iter()
                .map(|(k, v)| {
                    let summary = match v {
                        serde_json::Value::String(s) => {
                            format!("string({} chars)", s.chars().count())
                        }
                        serde_json::Value::Array(a) => format!("array({} items)", a.len()),
                        serde_json::Value::Object(o) => format!("object({} keys)", o.len()),
                        serde_json::Value::Null => "null".to_string(),
                        serde_json::Value::Bool(_) => "bool".to_string(),
                        serde_json::Value::Number(_) => "number".to_string(),
                    };
                    format!("{k}: {summary}")
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!("{{{fields}}}")
        }
        other => truncate_chars(&other.to_string(), 240),
    }
}

struct TruncatedBuffer {
    cap: usize,
    inner: Vec<u8>,
    truncated_head_bytes: usize,
}

impl TruncatedBuffer {
    fn new(cap: usize) -> Self {
        Self {
            cap,
            inner: Vec::new(),
            truncated_head_bytes: 0,
        }
    }

    fn push(&mut self, bytes: &[u8]) {
        self.inner.extend_from_slice(bytes);
        if self.inner.len() > self.cap {
            let drop = self.inner.len() - self.cap;
            self.inner.drain(..drop);
            self.truncated_head_bytes += drop;
        }
    }

    fn render(&self) -> String {
        let body = String::from_utf8_lossy(&self.inner);
        if self.truncated_head_bytes == 0 {
            body.into_owned()
        } else {
            format!(
                "[... truncated {} bytes from head ...]\n{}",
                self.truncated_head_bytes, body
            )
        }
    }
}

/// 启动 reader task：每读到一批字节就追加到 buffer + 更新 last_byte_at（驱动 idle 检测），
/// 同时（如果调用方传了 emit ctx）把这批字节作为 `agent-tool-stream` 事件发到前端。
fn spawn_pipe_reader<R>(
    mut pipe: R,
    buf: Arc<Mutex<TruncatedBuffer>>,
    last_byte_at: Arc<Mutex<Instant>>,
    emit: Option<(StreamCtx, String)>,
    evidence: Option<Arc<Mutex<EvidenceSink>>>,
) -> tokio::task::JoinHandle<()>
where
    R: AsyncReadExt + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut chunk = [0u8; SHELL_READ_CHUNK];
        loop {
            match pipe.read(&mut chunk).await {
                Ok(0) => {
                    if let Some((ctx, stream)) = &emit {
                        emit_stream_chunk(ctx, stream, "", true);
                    }
                    break;
                }
                Ok(n) => {
                    *last_byte_at.lock().unwrap() = Instant::now();
                    buf.lock().unwrap().push(&chunk[..n]);
                    if let Some(evidence) = &evidence {
                        evidence.lock().unwrap().write_chunk(&chunk[..n]);
                    }
                    if let Some((ctx, stream)) = &emit {
                        let text = String::from_utf8_lossy(&chunk[..n]).into_owned();
                        emit_stream_chunk(ctx, stream, &text, false);
                    }
                }
                Err(_) => {
                    if let Some((ctx, stream)) = &emit {
                        emit_stream_chunk(ctx, stream, "", true);
                    }
                    break;
                }
            }
        }
    })
}

/// 发送一段流式 chunk。失败默默忽略——发不出来不应阻塞 agent 工作。
fn emit_stream_chunk(ctx: &StreamCtx, stream: &str, chunk: &str, eof: bool) {
    let _ = ctx.app.emit(
        "agent-tool-stream",
        ToolStreamPayload {
            agent_id: ctx.agent_id.clone(),
            tool: "shell_exec".to_string(),
            stream: stream.to_string(),
            chunk: chunk.to_string(),
            eof,
        },
    );
}

/// 发一条 meta 元信息（命令开始 / spawn 失败 / watchdog kill 等），归入虚拟 stream "meta"。
fn emit_stream_meta(ctx: &StreamCtx, text: &str) {
    let _ = ctx.app.emit(
        "agent-tool-stream",
        ToolStreamPayload {
            agent_id: ctx.agent_id.clone(),
            tool: "shell_exec".to_string(),
            stream: "meta".to_string(),
            chunk: text.to_string(),
            eof: false,
        },
    );
}

/// 终止子进程：watchdog 触发的对象基本是死循环 / hang，直接 SIGKILL（tokio
/// `start_kill` 在 unix 等价于 SIGKILL）。`kill_on_drop` 已经做了二次保险。
async fn terminate_child(child: &mut tokio::process::Child) {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        unsafe {
            libc::killpg(pid as libc::pid_t, libc::SIGKILL);
        }
    }
    let _ = child.start_kill();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn setup() -> (TempDir, ToolExecutor) {
        let dir = TempDir::new().unwrap();
        let exec = ToolExecutor::new(dir.path().to_path_buf());
        (dir, exec)
    }

    #[tokio::test]
    async fn shell_large_output_writes_evidence_and_returns_manifest() {
        let (dir, exec) = setup();
        let ctx = ToolExecutionContext {
            agent_id: "agent-1".into(),
            step: 7,
            tool_use_id: "tool-1".into(),
            tool_name: "shell_exec".into(),
        };
        let out = exec
            .shell_exec(
                &serde_json::json!({
                    "command": "python3 - <<'PY'\nfor i in range(3000):\n    print(f'line {i}')\nprint('error: buried signal')\nPY"
                }),
                None,
                Some(ctx),
            )
            .await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("[shell_exec_output_compacted]"));
        assert!(out.content.contains("buried signal"));
        let meta = out.meta.expect("shell output should include evidence meta");
        let stdout_path = meta["stdout_path"].as_str().unwrap();
        let stdout = fs::read_to_string(dir.path().join(stdout_path)).unwrap();
        assert!(stdout.contains("line 0"));
        assert!(stdout.contains("error: buried signal"));
        assert_eq!(meta["compacted"], true);
    }

    #[tokio::test]
    async fn shell_html_manifest_ignores_css_error_noise() {
        let (dir, exec) = setup();
        let ctx = ToolExecutionContext {
            agent_id: "agent-1".into(),
            step: 8,
            tool_use_id: "tool-html".into(),
            tool_name: "shell_exec".into(),
        };
        let html = format!(
            "<!doctype html><html><head><title>Pricing Page</title><style>.error-text{{color:red}}</style></head><body><h1>Model Pricing</h1>{}</body></html>",
            " filler".repeat(30_000)
        );
        let script = format!("import sys\nsys.stdout.write({html:?})");
        let out = exec
            .shell_exec(
                &serde_json::json!({"command": format!("python3 - <<'PY'\n{script}\nPY")}),
                None,
                Some(ctx),
            )
            .await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("output_kind: html"), "{}", out.content);
        assert!(out.content.contains("Pricing Page") || out.content.contains("Model Pricing"));
        let high_signal = out
            .content
            .split("High-signal excerpts:")
            .nth(1)
            .and_then(|s| s.split("Suggested next steps:").next())
            .unwrap_or(&out.content);
        assert!(
            !high_signal.contains(".error-text"),
            "html CSS error noise leaked"
        );
        let meta = out.meta.expect("meta");
        assert_eq!(meta["output_kind"], "html");
        let stdout_path = meta["stdout_path"].as_str().unwrap();
        assert!(fs::read_to_string(dir.path().join(stdout_path))
            .unwrap()
            .contains(".error-text"));
    }
    #[tokio::test]
    async fn shell_small_output_keeps_content_and_adds_evidence_meta() {
        let (dir, exec) = setup();
        let ctx = ToolExecutionContext {
            agent_id: "agent-1".into(),
            step: 1,
            tool_use_id: "tool-2".into(),
            tool_name: "shell_exec".into(),
        };
        let out = exec
            .shell_exec(
                &serde_json::json!({"command": "printf hello"}),
                None,
                Some(ctx),
            )
            .await;
        assert!(!out.is_error);
        assert_eq!(out.content, "hello");
        let meta = out.meta.expect("shell output should include evidence meta");
        assert_eq!(meta["compacted"], false);
        assert_eq!(meta["command_family"], "generic");
        assert_eq!(meta["content_shape"], "text");
        let stdout_path = meta["stdout_path"].as_str().unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join(stdout_path)).unwrap(),
            "hello"
        );
    }

    #[test]
    fn shell_command_classifier_detects_content_commands() {
        assert_eq!(
            classify_shell_command_family("curl https://example.com/a.json"),
            "fetch"
        );
        assert_eq!(
            classify_shell_command_family("cat reports/out.md"),
            "file_dump"
        );
        assert_eq!(
            classify_shell_command_family("head -40 reports/out.md"),
            "file_excerpt"
        );
        assert_eq!(
            classify_shell_command_family("find . -type f"),
            "directory_listing"
        );
        assert_eq!(
            classify_shell_command_family("python3 -m pytest"),
            "build_test"
        );
        assert_eq!(
            command_content_source("curl https://example.com/a.json").as_deref(),
            Some("https://example.com/a.json")
        );
    }

    #[test]
    fn text_shape_classifier_detects_common_artifact_shapes() {
        assert_eq!(classify_text_shape("{\"ok\":true}"), "json");
        assert_eq!(classify_text_shape("<!doctype html><html></html>"), "html");
        assert_eq!(classify_text_shape("# Title\nbody"), "markdown");
        assert_eq!(classify_text_shape("a,b,c\n1,2,3"), "csv");
    }

    #[tokio::test]
    async fn shell_success_returns_stdout() {
        let (_dir, exec) = setup();
        let out = exec
            .execute("shell_exec", &serde_json::json!({"command": "echo hello"}))
            .await;
        assert!(!out.is_error);
        assert!(out.content.contains("hello"));
    }

    #[tokio::test]
    async fn shell_failure_returns_structured_error() {
        let (_dir, exec) = setup();
        let out = exec
            .execute("shell_exec", &serde_json::json!({"command": "exit 1"}))
            .await;
        assert!(out.is_error);
        let v: serde_json::Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(v["error"], "shell_error");
    }

    #[tokio::test]
    async fn shell_command_not_found() {
        let (_dir, exec) = setup();
        let out = exec
            .execute(
                "shell_exec",
                &serde_json::json!({"command": "totally_nonexistent_cmd_xyz"}),
            )
            .await;
        assert!(out.is_error);
        let v: serde_json::Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(v["error"], "shell_error");
        assert_eq!(v["exit_code"], 127);
        assert_eq!(v["shell_error_class"], "command_not_found");
        assert_eq!(v["capability_feedback"], true);
    }

    #[tokio::test]
    async fn read_file_not_found() {
        let (_dir, exec) = setup();
        let out = exec
            .execute("read_file", &serde_json::json!({"path": "nonexistent.txt"}))
            .await;
        assert!(out.is_error);
        let v: serde_json::Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(v["error"], "file_not_found");
    }

    #[tokio::test]
    async fn sandbox_violation() {
        let (_dir, exec) = setup();
        let out = exec
            .execute(
                "read_file",
                &serde_json::json!({"path": "../../etc/passwd"}),
            )
            .await;
        assert!(out.is_error);
        let v: serde_json::Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(v["error"], "sandbox_violation");
    }

    /// 模拟"长时间静默"——sleep 远超 idle 阈值。这里用 monkey-patch 之外的办法不太好，
    /// 但 sleep 70 是 1 分多钟会拖慢测试套件。改用 SHELL_DEFAULT_IDLE_SECS 假设最少为 60，
    /// 用 `tokio::time::pause` 配合 mock clock 太复杂；改为只验证：常规命令仍然能跑通、
    /// watchdog 不会误杀短任务。
    #[tokio::test]
    async fn shell_short_sleep_does_not_trip_watchdog() {
        let (_dir, exec) = setup();
        let out = exec
            .execute(
                "shell_exec",
                &serde_json::json!({"command": "sleep 1 && echo done"}),
            )
            .await;
        assert!(!out.is_error, "got error: {}", out.content);
        assert!(out.content.contains("done"));
    }

    /// 回归：mkdir -p 这种瞬时 + 零输出命令历史上偶发"卡住"——
    /// tokio multi-thread runtime 上 SIGCHLD 偶尔错过，wait() 不被唤醒，
    /// 又因为没有 stdout/stderr 字节驱动 watchdog 兜底。
    /// 本测试用 multi_thread runtime 显式覆盖该路径：tick 内的 try_wait
    /// 必须能在 ~100ms 内捕获到子进程退出。
    /// 同时跑 50 次累积概率：单次有 1% 漏唤醒就有 ~40% 概率被这个 loop 抓到。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn shell_instant_command_does_not_hang_on_multi_thread() {
        let (dir, exec) = setup();
        for i in 0..50 {
            let started = std::time::Instant::now();
            let out = exec
                .execute(
                    "shell_exec",
                    &serde_json::json!({
                        "command": format!("mkdir -p iter-{i}/sub")
                    }),
                )
                .await;
            let elapsed = started.elapsed();
            assert!(!out.is_error, "iter {i}: error: {}", out.content);
            assert!(
                elapsed < std::time::Duration::from_secs(2),
                "iter {i}: mkdir took {:?} (>2s — watchdog/SIGCHLD race not handled?)",
                elapsed
            );
            assert!(dir.path().join(format!("iter-{i}/sub")).is_dir());
        }
    }

    /// 显式声明 expect_long_running=true 时，超时阈值更高（这里只验证参数被接受、
    /// 短命令依然正常返回；真正的长任务超时验证用 dry-run 而非 sleep）。
    #[tokio::test]
    async fn shell_long_running_flag_accepted() {
        let (_dir, exec) = setup();
        let out = exec
            .execute(
                "shell_exec",
                &serde_json::json!({"command": "echo hi", "expect_long_running": true}),
            )
            .await;
        assert!(!out.is_error);
        assert!(out.content.contains("hi"));
    }

    #[tokio::test]
    async fn shell_timeout_kills_pipeline_process_group() {
        let (_dir, exec) = setup();
        let out = exec
            .shell_exec(
                &serde_json::json!({"command": "yes | python3 -c 'import sys, time; sys.stdin.read(1); time.sleep(5)'", "timeout_seconds": 1, "idle_timeout_seconds": 1}),
                None,
                None,
            )
            .await;
        assert!(out.is_error);
        let v: serde_json::Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(v["error"], "shell_killed");
        assert!(
            v["reason"].as_str().unwrap().contains("exceeded")
                || v["reason"].as_str().unwrap().contains("idle")
        );
    }

    #[tokio::test]
    async fn shell_explicit_timeout_seconds_is_enforced() {
        let (_dir, exec) = setup();
        let out = exec
            .execute(
                "shell_exec",
                &serde_json::json!({"command": "sleep 2", "timeout_seconds": 1}),
            )
            .await;
        assert!(out.is_error);
        let v: serde_json::Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(v["error"], "shell_killed");
        assert!(v["reason"]
            .as_str()
            .unwrap()
            .contains("wall_clock 1s exceeded"));
    }

    /// TruncatedBuffer：超过 cap 时丢头保尾。
    #[test]
    fn truncated_buffer_keeps_tail() {
        let mut buf = TruncatedBuffer::new(8);
        buf.push(b"0123456789ABCDEF");
        let s = buf.render();
        assert!(s.contains("89ABCDEF"));
        assert!(s.contains("truncated"));
    }

    #[tokio::test]
    async fn non_shell_large_output_persists_tool_result_with_context() {
        let (_dir, exec) = setup();
        let original = "x".repeat(3 * 1024);
        let out = exec.attach_persisted_tool_result(
            ToolOutput {
                content: original.clone(),
                is_error: false,
                meta: None,
            },
            Some(&ToolExecutionContext {
                agent_id: "agent-large".to_string(),
                step: 2,
                tool_use_id: "read-large".to_string(),
                tool_name: "read_file".to_string(),
            }),
        );

        assert!(!out.is_error, "got error: {}", out.content);
        let meta = out.meta.expect("large non-shell output should have meta");
        let persisted_path = meta["persisted_path"]
            .as_str()
            .expect("persisted path should be present");
        assert!(persisted_path.contains("agent-large"));
        assert!(persisted_path.contains("read-large"));
        let persisted = fs::read_to_string(persisted_path).unwrap();
        assert_eq!(persisted, original);
        assert_eq!(meta["kind"], "persisted_tool_result");
    }

    #[tokio::test]
    async fn write_file_success() {
        let (dir, exec) = setup();
        let out = exec
            .execute(
                "write_file",
                &serde_json::json!({"path": "test.txt", "content": "hello world"}),
            )
            .await;
        assert!(!out.is_error);
        let content = fs::read_to_string(dir.path().join("test.txt")).unwrap();
        assert_eq!(content, "hello world");
    }

    #[tokio::test]
    async fn write_file_appends_chunks() {
        let (dir, exec) = setup();
        let first = exec
            .execute(
                "write_file",
                &serde_json::json!({"path": "chunked.py", "content": "print('a')\n"}),
            )
            .await;
        assert!(!first.is_error, "got error: {}", first.content);
        let second = exec
            .execute(
                "write_file",
                &serde_json::json!({"path": "chunked.py", "content": "print('b')\n", "append": true}),
            )
            .await;
        assert!(!second.is_error, "got error: {}", second.content);
        let content = fs::read_to_string(dir.path().join("chunked.py")).unwrap();
        assert_eq!(content, "print('a')\nprint('b')\n");
    }

    #[tokio::test]
    async fn write_file_accepts_file_path_alias() {
        let (dir, exec) = setup();
        let out = exec
            .execute(
                "write_file",
                &serde_json::json!({"file_path": "alias.txt", "content": "hello alias"}),
            )
            .await;
        assert!(!out.is_error, "got error: {}", out.content);
        let content = fs::read_to_string(dir.path().join("alias.txt")).unwrap();
        assert_eq!(content, "hello alias");
    }

    #[tokio::test]
    async fn write_file_rejects_compacted_history_stub() {
        let (_dir, exec) = setup();
        let out = exec
            .execute(
                "write_file",
                &serde_json::json!({
                    "__tool_use_input_compacted__": true,
                    "original_path": "big.py",
                    "__tool_use_input_excerpt__": "{\"path\":\"big.py\",\"content\":\"..."
                }),
            )
            .await;
        assert!(out.is_error);
        assert!(out
            .content
            .contains("compacted historical tool_use input stub"));
        assert!(out.content.contains("explicit path/content"));
    }

    #[tokio::test]
    async fn notebook_edit_reads_cells() {
        let (dir, exec) = setup();
        fs::write(
            dir.path().join("analysis.ipynb"),
            serde_json::json!({
                "cells": [
                    {
                        "cell_type": "code",
                        "metadata": {},
                        "execution_count": null,
                        "outputs": [],
                        "source": ["summary = {'ok': True}\n"]
                    }
                ],
                "metadata": {},
                "nbformat": 4,
                "nbformat_minor": 5
            })
            .to_string(),
        )
        .unwrap();

        let out = exec
            .execute(
                "notebook_edit",
                &serde_json::json!({"path": "analysis.ipynb", "operation": "read_cells"}),
            )
            .await;
        assert!(!out.is_error, "got error: {}", out.content);
        let v: serde_json::Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(v["cell_count"], 1);
        assert_eq!(v["cells"][0]["source"], "summary = {'ok': True}\n");
    }

    #[tokio::test]
    async fn notebook_edit_read_cells_truncates_large_sources() {
        let (dir, exec) = setup();
        fs::write(
            dir.path().join("large.ipynb"),
            serde_json::json!({
                "cells": [
                    {
                        "cell_type": "code",
                        "metadata": {},
                        "execution_count": null,
                        "outputs": [],
                        "source": [format!("{}TAIL\n", "x".repeat(NOTEBOOK_CELL_SOURCE_MAX_CHARS + 512))]
                    },
                    {
                        "cell_type": "markdown",
                        "metadata": {},
                        "source": ["short\n"]
                    }
                ],
                "metadata": {},
                "nbformat": 4,
                "nbformat_minor": 5
            })
            .to_string(),
        )
        .unwrap();

        let out = exec
            .execute(
                "notebook_edit",
                &serde_json::json!({"path": "large.ipynb", "operation": "read_cells", "limit": 1}),
            )
            .await;
        assert!(!out.is_error, "got error: {}", out.content);
        let v: serde_json::Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(v["returned"], 1);
        assert_eq!(v["truncated_cells"], 1);
        assert_eq!(v["truncated_sources"], 1);
        assert_eq!(v["cells"][0]["source_truncated"], true);
        assert!(v["cells"][0]["source"].as_str().unwrap().contains('…'));
        assert!(v["hint"].as_str().unwrap().contains("Large cell sources"));
    }

    #[tokio::test]
    async fn notebook_edit_inserts_cell_after_source_match() {
        let (dir, exec) = setup();
        fs::write(
            dir.path().join("analysis.ipynb"),
            serde_json::json!({
                "cells": [
                    {
                        "cell_type": "code",
                        "metadata": {},
                        "execution_count": null,
                        "outputs": [],
                        "source": ["summary = {'ok': True}\n"]
                    }
                ],
                "metadata": {},
                "nbformat": 4,
                "nbformat_minor": 5
            })
            .to_string(),
        )
        .unwrap();

        let out = exec
            .execute(
                "notebook_edit",
                &serde_json::json!({
                    "path": "analysis.ipynb",
                    "operation": "insert_cell",
                    "after_source_contains": "summary =",
                    "cell_type": "code",
                    "source": "release_snapshot = {'status': 'ready'}\nrelease_snapshot\n"
                }),
            )
            .await;
        assert!(!out.is_error, "got error: {}", out.content);
        let v: serde_json::Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(v["inserted_index"], 1);

        let notebook: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(dir.path().join("analysis.ipynb")).unwrap())
                .unwrap();
        assert_eq!(notebook["cells"].as_array().unwrap().len(), 2);
        assert_eq!(
            notebook["cells"][1]["source"][0],
            "release_snapshot = {'status': 'ready'}\n"
        );
        assert_eq!(notebook["cells"][1]["source"][1], "release_snapshot\n");
    }

    /// 没读过文件就 edit → 拒绝。这是 edit_without_read 不变量的回归测试。
    #[tokio::test]
    async fn edit_without_prior_read_is_rejected() {
        let (dir, exec) = setup();
        fs::write(dir.path().join("a.txt"), "hello world").unwrap();
        let out = exec
            .execute(
                "edit_file",
                &serde_json::json!({
                    "path": "a.txt",
                    "old_string": "world",
                    "new_string": "rust",
                }),
            )
            .await;
        assert!(out.is_error);
        let v: serde_json::Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(v["error"], "edit_without_read");
        // 文件没被改
        assert_eq!(
            fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "hello world"
        );
    }

    /// 读过 → 唯一替换 → 成功并落盘 + 返回结构化 payload。
    #[tokio::test]
    async fn edit_after_read_unique_replacement_succeeds() {
        let (dir, exec) = setup();
        fs::write(dir.path().join("a.txt"), "hello world").unwrap();

        let _ = exec
            .execute("read_file", &serde_json::json!({"path": "a.txt"}))
            .await;

        let out = exec
            .execute(
                "edit_file",
                &serde_json::json!({
                    "path": "a.txt",
                    "old_string": "world",
                    "new_string": "rust",
                }),
            )
            .await;
        assert!(!out.is_error, "got error: {}", out.content);
        let v: serde_json::Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(v["replacements"], 1);
        assert_eq!(
            fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "hello rust"
        );
    }

    /// 多处匹配 + replace_all=false → 拒绝，让 LLM 加 context 重试。
    #[tokio::test]
    async fn edit_non_unique_without_replace_all_is_rejected() {
        let (dir, exec) = setup();
        fs::write(dir.path().join("b.txt"), "x x x").unwrap();
        let _ = exec
            .execute("read_file", &serde_json::json!({"path": "b.txt"}))
            .await;

        let out = exec
            .execute(
                "edit_file",
                &serde_json::json!({
                    "path": "b.txt",
                    "old_string": "x",
                    "new_string": "y",
                }),
            )
            .await;
        assert!(out.is_error);
        let v: serde_json::Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(v["error"], "edit_not_unique");
        assert!(v["message"].as_str().unwrap().contains("3 times"));
        // 文件未改
        assert_eq!(
            fs::read_to_string(dir.path().join("b.txt")).unwrap(),
            "x x x"
        );
    }

    /// replace_all=true → 全部替换。
    #[tokio::test]
    async fn edit_replace_all_replaces_every_occurrence() {
        let (dir, exec) = setup();
        fs::write(dir.path().join("c.txt"), "foo foo foo").unwrap();
        let _ = exec
            .execute("read_file", &serde_json::json!({"path": "c.txt"}))
            .await;

        let out = exec
            .execute(
                "edit_file",
                &serde_json::json!({
                    "path": "c.txt",
                    "old_string": "foo",
                    "new_string": "bar",
                    "replace_all": true,
                }),
            )
            .await;
        assert!(!out.is_error);
        let v: serde_json::Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(v["replacements"], 3);
        assert_eq!(
            fs::read_to_string(dir.path().join("c.txt")).unwrap(),
            "bar bar bar"
        );
    }

    /// old_string 不存在 → 拒绝。
    #[tokio::test]
    async fn edit_no_match_is_rejected() {
        let (dir, exec) = setup();
        fs::write(dir.path().join("d.txt"), "hello").unwrap();
        let _ = exec
            .execute("read_file", &serde_json::json!({"path": "d.txt"}))
            .await;

        let out = exec
            .execute(
                "edit_file",
                &serde_json::json!({
                    "path": "d.txt",
                    "old_string": "world",
                    "new_string": "rust",
                }),
            )
            .await;
        assert!(out.is_error);
        let v: serde_json::Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(v["error"], "edit_no_match");
    }

    /// edit 让文件变空 → 拒绝（防误删兜底）。
    #[tokio::test]
    async fn edit_that_blanks_the_file_is_rejected() {
        let (dir, exec) = setup();
        fs::write(dir.path().join("e.txt"), "content").unwrap();
        let _ = exec
            .execute("read_file", &serde_json::json!({"path": "e.txt"}))
            .await;

        let out = exec
            .execute(
                "edit_file",
                &serde_json::json!({
                    "path": "e.txt",
                    "old_string": "content",
                    "new_string": "",
                }),
            )
            .await;
        assert!(out.is_error);
        let v: serde_json::Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(v["error"], "edit_would_blank_file");
        // 原文件保留
        assert_eq!(
            fs::read_to_string(dir.path().join("e.txt")).unwrap(),
            "content"
        );
    }

    // ---- Single-Agent Uplift Phase 1.3: glob 单测 ----

    #[tokio::test]
    async fn glob_finds_files_by_extension() {
        let (dir, exec) = setup();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/a.rs"), "x").unwrap();
        fs::write(dir.path().join("src/b.rs"), "x").unwrap();
        fs::write(dir.path().join("src/c.txt"), "x").unwrap();

        let out = exec
            .execute("glob", &serde_json::json!({"pattern": "**/*.rs"}))
            .await;
        assert!(!out.is_error, "got error: {}", out.content);
        assert!(out.content.contains("a.rs"));
        assert!(out.content.contains("b.rs"));
        assert!(!out.content.contains("c.txt"));
    }

    #[tokio::test]
    async fn glob_reports_truncation_with_next_step_hint() {
        let (dir, exec) = setup();
        for i in 0..5 {
            fs::write(dir.path().join(format!("g-{i}.rs")), "x").unwrap();
        }

        let out = exec
            .execute("glob", &serde_json::json!({"pattern": "*.rs", "limit": 2}))
            .await;
        assert!(!out.is_error, "got: {}", out.content);
        assert!(
            out.content.contains("glob truncated"),
            "got:\n{}",
            out.content
        );
        assert!(
            out.content.contains("5 total matches"),
            "got:\n{}",
            out.content
        );
        assert!(
            out.content.contains("narrower pattern/path"),
            "got:\n{}",
            out.content
        );
    }

    #[tokio::test]
    async fn glob_empty_match_returns_friendly_message() {
        let (_dir, exec) = setup();
        let out = exec
            .execute("glob", &serde_json::json!({"pattern": "**/*.nonexistent"}))
            .await;
        assert!(!out.is_error);
        assert!(out.content.contains("No files matched"));
    }

    #[tokio::test]
    async fn glob_invalid_pattern_returns_parameter_error() {
        let (_dir, exec) = setup();
        let out = exec
            .execute("glob", &serde_json::json!({"pattern": "[unclosed"}))
            .await;
        assert!(out.is_error);
        let v: serde_json::Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(v["error"], "parameter_error");
    }

    // ---- A1: read_file 行号 + offset/limit ----

    #[tokio::test]
    async fn read_file_emits_line_numbers() {
        let (dir, exec) = setup();
        fs::write(dir.path().join("nums.txt"), "alpha\nbeta\ngamma\n").unwrap();
        let out = exec
            .execute("read_file", &serde_json::json!({"path": "nums.txt"}))
            .await;
        assert!(!out.is_error, "expected ok, got: {}", out.content);
        // 行号格式：右对齐 6 位 + |
        assert!(
            out.content.contains("     1|alpha"),
            "got:\n{}",
            out.content
        );
        assert!(out.content.contains("     2|beta"));
        assert!(out.content.contains("     3|gamma"));
    }

    #[tokio::test]
    async fn read_file_offset_limit_pages_through_file() {
        let (dir, exec) = setup();
        let big: String = (1..=50).map(|i| format!("line{i}\n")).collect();
        fs::write(dir.path().join("big.txt"), &big).unwrap();

        let out = exec
            .execute(
                "read_file",
                &serde_json::json!({"path": "big.txt", "offset": 10, "limit": 3}),
            )
            .await;
        assert!(!out.is_error);
        assert!(
            out.content.contains("    10|line10"),
            "got:\n{}",
            out.content
        );
        assert!(out.content.contains("    11|line11"));
        assert!(out.content.contains("    12|line12"));
        assert!(
            !out.content.contains("    13|line13"),
            "should not include past limit"
        );
        // 应当告知有截断（end_idx=12 < 50）
        assert!(out.content.contains("truncated at line 12 of 50"));
    }

    #[tokio::test]
    async fn read_file_offset_beyond_eof_returns_notice() {
        let (dir, exec) = setup();
        fs::write(dir.path().join("short.txt"), "alpha\nbeta\n").unwrap();

        let out = exec
            .execute(
                "read_file",
                &serde_json::json!({"path": "short.txt", "offset": 99, "limit": 3}),
            )
            .await;

        assert!(!out.is_error, "got: {}", out.content);
        assert!(out
            .content
            .contains("requested offset 99 is beyond end of file"));
        assert!(out.content.contains("file has 2 lines"));
    }
    #[tokio::test]
    async fn read_file_truncates_extremely_long_lines() {
        let (dir, exec) = setup();
        let long_line = format!("{}TAIL", "a".repeat(READ_FILE_MAX_LINE_CHARS + 512));
        fs::write(dir.path().join("long-line.txt"), &long_line).unwrap();

        let out = exec
            .execute(
                "read_file",
                &serde_json::json!({"path": "long-line.txt", "limit": 1}),
            )
            .await;
        assert!(!out.is_error, "got: {}", out.content);
        assert!(out.content.contains('…'), "got:\n{}", out.content);
        assert!(out.content.contains("TAIL"), "got:\n{}", out.content);
        assert!(out.content.chars().count() < long_line.chars().count());
    }

    #[tokio::test]
    async fn read_file_caps_total_output_with_hint() {
        let (dir, exec) = setup();
        let content = (0..3000)
            .map(|i| format!("line-{i:04}-{}\n", "x".repeat(80)))
            .collect::<String>();
        fs::write(dir.path().join("huge.txt"), content).unwrap();

        let out = exec
            .execute(
                "read_file",
                &serde_json::json!({"path": "huge.txt", "limit": 5000}),
            )
            .await;
        assert!(!out.is_error, "got: {}", out.content);
        assert!(
            out.content.contains("truncated to"),
            "got:\n{}",
            out.content
        );
        assert!(
            out.content
                .contains("use a smaller limit/offset window or grep for a narrower pattern"),
            "got:\n{}",
            out.content
        );
    }

    #[tokio::test]
    async fn parameter_errors_summarize_large_inputs() {
        let (_dir, exec) = setup();
        let huge = "x".repeat(20_000);

        let out = exec
            .execute("read_file", &serde_json::json!({"content": huge.clone()}))
            .await;
        assert!(out.is_error);
        assert!(out.content.contains("content: string(20000 chars)"));
        assert!(!out.content.contains(&"x".repeat(1000)));

        let out = exec
            .execute("shell_exec", &serde_json::json!({"content": huge.clone()}))
            .await;
        assert!(out.is_error);
        assert!(out.content.contains("content: string(20000 chars)"));
        assert!(!out.content.contains(&"x".repeat(1000)));

        let out = exec
            .execute("write_file", &serde_json::json!({"content": huge.clone()}))
            .await;
        assert!(out.is_error);
        assert!(out.content.contains("content: string(20000 chars)"));
        assert!(!out.content.contains(&"x".repeat(1000)));

        let out = exec
            .execute(
                "edit_file",
                &serde_json::json!({"old_string": huge.clone()}),
            )
            .await;
        assert!(out.is_error);
        assert!(out.content.contains("old_string: string(20000 chars)"));
        assert!(!out.content.contains(&"x".repeat(1000)));
    }

    // ---- A2: file_unchanged_since_last_read stub ----

    #[tokio::test]
    async fn read_file_returns_unchanged_stub_on_repeat_full_read() {
        let (dir, exec) = setup();
        fs::write(dir.path().join("once.txt"), "alpha\nbeta\n").unwrap();
        let _ = exec
            .execute("read_file", &serde_json::json!({"path": "once.txt"}))
            .await;
        // 第二次 read：mtime 未变 → 返回 stub
        let out = exec
            .execute("read_file", &serde_json::json!({"path": "once.txt"}))
            .await;
        assert!(!out.is_error);
        let v: serde_json::Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(v["file_unchanged_since_last_read"], true);
        assert_eq!(v["path"], "once.txt");
    }

    #[tokio::test]
    async fn read_file_paged_request_skips_unchanged_stub() {
        let (dir, exec) = setup();
        fs::write(dir.path().join("p.txt"), "a\nb\nc\n").unwrap();
        let _ = exec
            .execute("read_file", &serde_json::json!({"path": "p.txt"}))
            .await;
        // 即便 mtime 未变，分页请求依然走完整 read。
        let out = exec
            .execute(
                "read_file",
                &serde_json::json!({"path": "p.txt", "offset": 1, "limit": 2}),
            )
            .await;
        assert!(!out.is_error);
        assert!(out.content.contains("     1|a"));
        // 不是 stub
        assert!(!out.content.contains("file_unchanged_since_last_read"));
    }

    // ---- write_file -> edit_file 不再要求重读（修复 P1 bug） ----

    #[tokio::test]
    async fn write_file_then_edit_file_no_reread_required() {
        let (dir, exec) = setup();
        let out_w = exec
            .execute(
                "write_file",
                &serde_json::json!({"path": "freshly_written.txt", "content": "hello world"}),
            )
            .await;
        assert!(!out_w.is_error);
        let out_e = exec
            .execute(
                "edit_file",
                &serde_json::json!({
                    "path": "freshly_written.txt",
                    "old_string": "hello",
                    "new_string": "goodbye",
                }),
            )
            .await;
        assert!(
            !out_e.is_error,
            "edit_file should accept just-written file: {}",
            out_e.content
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("freshly_written.txt")).unwrap(),
            "goodbye world"
        );
    }

    // ---- B3: edit_file multi-edit ----

    #[tokio::test]
    async fn edit_file_multi_edit_applies_in_order() {
        let (dir, exec) = setup();
        fs::write(dir.path().join("m.txt"), "alpha beta gamma").unwrap();
        let _ = exec
            .execute("read_file", &serde_json::json!({"path": "m.txt"}))
            .await;

        let out = exec
            .execute(
                "edit_file",
                &serde_json::json!({
                    "path": "m.txt",
                    "edits": [
                        {"old_string": "alpha", "new_string": "ALPHA"},
                        {"old_string": "gamma", "new_string": "GAMMA"},
                    ]
                }),
            )
            .await;
        assert!(!out.is_error, "got: {}", out.content);
        let v: serde_json::Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(v["edits_applied"], 2);
        assert_eq!(v["replacements"], 2);
        assert_eq!(
            fs::read_to_string(dir.path().join("m.txt")).unwrap(),
            "ALPHA beta GAMMA"
        );
    }

    #[tokio::test]
    async fn edit_file_multi_edit_atomic_on_failure() {
        let (dir, exec) = setup();
        fs::write(dir.path().join("a.txt"), "alpha beta gamma").unwrap();
        let _ = exec
            .execute("read_file", &serde_json::json!({"path": "a.txt"}))
            .await;

        let out = exec
            .execute(
                "edit_file",
                &serde_json::json!({
                    "path": "a.txt",
                    "edits": [
                        {"old_string": "alpha", "new_string": "ALPHA"},
                        {"old_string": "no_such_token", "new_string": "x"},
                    ]
                }),
            )
            .await;
        assert!(out.is_error);
        // 文件应保持原样（第一条 edit 已应用但未写盘 → atomic）
        assert_eq!(
            fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "alpha beta gamma"
        );
    }

    // ---- B4: edit_file desanitize / line-number-prefix fallback ----

    #[tokio::test]
    async fn edit_file_strips_line_number_prefix_in_old_string() {
        let (dir, exec) = setup();
        fs::write(
            dir.path().join("ln.txt"),
            "fn foo() {\n    println!(\"hi\");\n}\n",
        )
        .unwrap();
        let _ = exec
            .execute("read_file", &serde_json::json!({"path": "ln.txt"}))
            .await;

        // LLM 直接从 read_file 输出复制粘贴，带行号前缀
        let out = exec
            .execute(
                "edit_file",
                &serde_json::json!({
                    "path": "ln.txt",
                    "old_string": "     2|    println!(\"hi\");",
                    "new_string": "     2|    println!(\"yo\");",
                }),
            )
            .await;
        assert!(!out.is_error, "got: {}", out.content);
        let updated = fs::read_to_string(dir.path().join("ln.txt")).unwrap();
        assert!(updated.contains("println!(\"yo\")"), "got:\n{}", updated);
        assert!(!updated.contains("println!(\"hi\")"));
    }

    #[tokio::test]
    async fn edit_file_handles_curly_quotes() {
        let (dir, exec) = setup();
        // 文件里是 ASCII 双引号
        fs::write(dir.path().join("q.txt"), "let x = \"hello\";\n").unwrap();
        let _ = exec
            .execute("read_file", &serde_json::json!({"path": "q.txt"}))
            .await;

        // LLM 给的 old_string 是 curly quote
        let out = exec
            .execute(
                "edit_file",
                &serde_json::json!({
                    "path": "q.txt",
                    "old_string": "let x = \u{201C}hello\u{201D};",
                    "new_string": "let x = \"world\";",
                }),
            )
            .await;
        assert!(!out.is_error, "got: {}", out.content);
        assert_eq!(
            fs::read_to_string(dir.path().join("q.txt")).unwrap(),
            "let x = \"world\";\n"
        );
    }

    #[tokio::test]
    async fn default_search_glob_and_list_skip_internal_evidence_dirs() {
        let (dir, exec) = setup();
        fs::write(dir.path().join("visible.txt"), "needle\n").unwrap();
        fs::create_dir_all(dir.path().join(".miragenty-evidence/run/step")).unwrap();
        fs::write(
            dir.path().join(".miragenty-evidence/run/step/stdout.txt"),
            "needle hidden\n",
        )
        .unwrap();

        let grep = exec
            .execute("grep", &serde_json::json!({"pattern": "needle"}))
            .await;
        assert!(!grep.is_error, "got: {}", grep.content);
        assert!(
            grep.content.contains("visible.txt"),
            "got:\n{}",
            grep.content
        );
        assert!(
            !grep.content.contains(".miragenty-evidence"),
            "got:\n{}",
            grep.content
        );

        let glob = exec
            .execute("glob", &serde_json::json!({"pattern": "**/*.txt"}))
            .await;
        assert!(!glob.is_error, "got: {}", glob.content);
        assert!(
            glob.content.contains("visible.txt"),
            "got:\n{}",
            glob.content
        );
        assert!(
            !glob.content.contains(".miragenty-evidence"),
            "got:\n{}",
            glob.content
        );

        let list = exec
            .execute("list_files", &serde_json::json!({"path": "."}))
            .await;
        assert!(!list.is_error, "got: {}", list.content);
        assert!(
            !list.content.contains(".miragenty-evidence"),
            "got:\n{}",
            list.content
        );
    }

    #[tokio::test]
    async fn default_discovery_skips_mirrored_benchmark_assets() {
        let (dir, exec) = setup();
        let workspace_name = dir
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string();
        fs::write(dir.path().join("README.md"), "needle visible\n").unwrap();
        let mirrored_dir = dir.path().join("assets").join(&workspace_name);
        fs::create_dir_all(&mirrored_dir).unwrap();
        fs::write(mirrored_dir.join("README.md"), "needle mirrored\n").unwrap();

        let grep = exec
            .execute("grep", &serde_json::json!({"pattern": "needle"}))
            .await;
        assert!(!grep.is_error, "got: {}", grep.content);
        assert!(grep.content.contains("README.md"), "got:\n{}", grep.content);
        assert!(!grep.content.contains("assets/"), "got:\n{}", grep.content);

        let glob = exec
            .execute("glob", &serde_json::json!({"pattern": "**/*.md"}))
            .await;
        assert!(!glob.is_error, "got: {}", glob.content);
        assert!(glob.content.contains("README.md"), "got:\n{}", glob.content);
        assert!(!glob.content.contains("assets/"), "got:\n{}", glob.content);

        let root_list = exec
            .execute("list_files", &serde_json::json!({"path": "."}))
            .await;
        assert!(!root_list.is_error, "got: {}", root_list.content);
        assert!(
            root_list.content.contains("README.md"),
            "got:\n{}",
            root_list.content
        );
        assert!(
            !root_list.content.contains("assets/"),
            "got:\n{}",
            root_list.content
        );

        let list = exec
            .execute("list_files", &serde_json::json!({"path": "assets"}))
            .await;
        assert!(!list.is_error, "got: {}", list.content);
        assert!(
            !list.content.contains(&workspace_name),
            "got:\n{}",
            list.content
        );

        let explicit = exec
            .execute(
                "read_file",
                &serde_json::json!({"path": format!("assets/{workspace_name}/README.md")}),
            )
            .await;
        assert!(!explicit.is_error, "got: {}", explicit.content);
        assert!(
            explicit.content.contains("needle mirrored"),
            "got:\n{}",
            explicit.content
        );
    }

    #[tokio::test]
    async fn explicit_evidence_path_can_be_read_grepped_and_listed() {
        let (dir, exec) = setup();
        let evidence_root = dir.path().join("evidence-root");
        fs::create_dir_all(&evidence_root).unwrap();
        fs::write(
            evidence_root.join("stdout.txt"),
            "needle evidence\n".repeat(700),
        )
        .unwrap();
        let exec = exec.with_evidence_root(evidence_root.clone());
        let evidence_path = evidence_root.join("stdout.txt").display().to_string();
        let evidence_dir = evidence_root.display().to_string();

        let read = exec
            .execute("read_file", &serde_json::json!({"path": evidence_path}))
            .await;
        assert!(!read.is_error, "got: {}", read.content);
        assert!(
            read.content.starts_with("[evidence_read_ref]"),
            "got:\n{}",
            read.content
        );
        assert!(read.content.contains("offset"), "got:\n{}", read.content);

        let grep = exec
            .execute(
                "grep",
                &serde_json::json!({"path": evidence_dir, "pattern": "needle", "head_limit": 2}),
            )
            .await;
        assert!(!grep.is_error, "got: {}", grep.content);
        assert!(
            grep.content.contains("stdout.txt"),
            "got:\n{}",
            grep.content
        );

        let list = exec
            .execute(
                "list_files",
                &serde_json::json!({"path": evidence_root.display().to_string()}),
            )
            .await;
        assert!(!list.is_error, "got: {}", list.content);
        assert!(
            list.content.contains("stdout.txt"),
            "got:\n{}",
            list.content
        );
    }

    #[tokio::test]
    async fn list_files_respects_limit_and_reports_truncation() {
        let (dir, exec) = setup();
        for i in 0..5 {
            fs::write(dir.path().join(format!("file-{i}.txt")), "x").unwrap();
        }

        let out = exec
            .execute("list_files", &serde_json::json!({"path": ".", "limit": 2}))
            .await;
        assert!(!out.is_error, "got: {}", out.content);
        assert!(
            out.content.contains("entries_shown=2"),
            "got:\n{}",
            out.content
        );
        assert!(
            out.content.contains("entries_total=5"),
            "got:\n{}",
            out.content
        );
        assert!(
            out.content.contains("truncated=true"),
            "got:\n{}",
            out.content
        );
        assert!(
            out.content.contains("truncated 3 entries"),
            "got:\n{}",
            out.content
        );
    }

    #[tokio::test]
    async fn search_files_files_with_matches_mode() {
        let (dir, exec) = setup();
        fs::write(dir.path().join("a.rs"), "let foo = 1;\n").unwrap();
        fs::write(dir.path().join("b.rs"), "let bar = 2;\n").unwrap();
        fs::write(dir.path().join("c.txt"), "let foo = 3;\n").unwrap();

        let out = exec
            .execute(
                "search_files",
                &serde_json::json!({"pattern": "foo", "output_mode": "files_with_matches"}),
            )
            .await;
        assert!(!out.is_error, "got: {}", out.content);
        assert!(out.content.contains("a.rs"), "got:\n{}", out.content);
        assert!(out.content.contains("c.txt"));
        assert!(!out.content.contains("b.rs"));
    }

    #[tokio::test]
    async fn search_files_glob_filters() {
        let (dir, exec) = setup();
        fs::write(dir.path().join("x.rs"), "needle here\n").unwrap();
        fs::write(dir.path().join("x.txt"), "needle here\n").unwrap();

        let out = exec
            .execute(
                "search_files",
                &serde_json::json!({
                    "pattern": "needle",
                    "glob": "*.rs",
                    "output_mode": "files_with_matches"
                }),
            )
            .await;
        assert!(!out.is_error);
        assert!(out.content.contains("x.rs"));
        assert!(!out.content.contains("x.txt"));
    }

    #[tokio::test]
    async fn search_files_no_match_returns_friendly_message() {
        let (dir, exec) = setup();
        fs::write(dir.path().join("x.txt"), "alpha\n").unwrap();
        let out = exec
            .execute(
                "search_files",
                &serde_json::json!({"pattern": "definitely_not_present_here"}),
            )
            .await;
        assert!(!out.is_error);
        assert!(out.content.contains("No matches"), "got: {}", out.content);
    }

    // ---- grep 主名路径（2026-05 重命名后 LLM 实际看到/调用的就是这个名字）----
    // 上面三个 search_files_* 测试现在等同于 "alias 路径回归"——确保旧 session
    // replay / 旧 hook config 仍走通。这里加 3 个 grep_* 测试验证主名路径。

    #[tokio::test]
    async fn grep_content_mode_returns_line_numbers() {
        let (dir, exec) = setup();
        fs::write(
            dir.path().join("a.rs"),
            "let x = 1;\nlet needle = 2;\nlet y = 3;\n",
        )
        .unwrap();
        let out = exec
            .execute("grep", &serde_json::json!({"pattern": "needle"}))
            .await;
        assert!(!out.is_error, "got: {}", out.content);
        // content mode 默认带 --line-number：应能看到 `a.rs:2:let needle = 2;`
        assert!(out.content.contains("a.rs"), "got:\n{}", out.content);
        assert!(out.content.contains("2"), "should have line number 2");
        assert!(out.content.contains("needle"));
    }

    #[tokio::test]
    async fn grep_count_mode_aggregates_per_file() {
        let (dir, exec) = setup();
        fs::write(dir.path().join("a.rs"), "foo\nfoo\nbar\n").unwrap();
        fs::write(dir.path().join("b.rs"), "foo\n").unwrap();
        let out = exec
            .execute(
                "grep",
                &serde_json::json!({"pattern": "foo", "output_mode": "count"}),
            )
            .await;
        assert!(!out.is_error, "got: {}", out.content);
        // count 模式 rg 输出形如 `a.rs:2\nb.rs:1\n`
        assert!(
            out.content.contains("a.rs:2"),
            "expected a.rs count=2, got:\n{}",
            out.content
        );
        assert!(out.content.contains("b.rs:1"), "got:\n{}", out.content);
    }

    #[tokio::test]
    async fn grep_truncates_long_matching_lines() {
        let (dir, exec) = setup();
        let line = format!("needle-{}-TAIL\n", "x".repeat(GREP_MAX_LINE_CHARS + 512));
        fs::write(dir.path().join("long.txt"), line).unwrap();

        let out = exec
            .execute("grep", &serde_json::json!({"pattern": "needle"}))
            .await;
        assert!(!out.is_error, "got: {}", out.content);
        assert!(out.content.contains('…'), "got:\n{}", out.content);
        assert!(out.content.contains("TAIL"), "got:\n{}", out.content);
        assert!(out.content.chars().count() < GREP_MAX_LINE_CHARS + 512);
    }

    #[tokio::test]
    async fn grep_head_limit_stops_after_requested_lines() {
        let (dir, exec) = setup();
        fs::write(dir.path().join("many.txt"), "needle\n".repeat(1000)).unwrap();

        let out = exec
            .execute(
                "grep",
                &serde_json::json!({"pattern": "needle", "head_limit": 3}),
            )
            .await;
        assert!(!out.is_error, "got: {}", out.content);
        assert_eq!(
            out.content
                .lines()
                .filter(|line| line.contains("needle"))
                .count(),
            3
        );
        assert!(
            out.content.contains("truncated after 3 lines"),
            "got:\n{}",
            out.content
        );
    }

    /// 关键回归：通过主名 `grep` 调与通过 alias `search_files` 调，行为必须**字节相等**。
    /// 否则 alias 兼容性是假的。
    #[tokio::test]
    async fn grep_and_search_files_alias_produce_identical_output() {
        let (dir, exec) = setup();
        fs::write(dir.path().join("file1.rs"), "hello world\n").unwrap();
        fs::write(dir.path().join("file2.rs"), "hello again\n").unwrap();

        let args = serde_json::json!({
            "pattern": "hello",
            "output_mode": "files_with_matches"
        });
        let via_grep = exec.execute("grep", &args).await;
        let via_alias = exec.execute("search_files", &args).await;

        assert_eq!(via_grep.is_error, via_alias.is_error);
        assert_eq!(
            via_grep.content, via_alias.content,
            "grep main-name and search_files alias must produce identical output"
        );
    }

    // ---- 行号前缀检测的退化情况：含 `|` 的 Rust 模式不应被误认为行号块 ----

    #[test]
    fn line_number_prefix_does_not_eat_pattern_match_lines() {
        // 多行 Rust pattern：第一行像行号 `42|` 但第二行不是
        let s = "    42|something\nSome(x) | None => {}";
        assert!(strip_line_number_prefix(s).is_none());
    }

    #[test]
    fn line_number_prefix_recognises_real_block() {
        let s = "    42|something\n    43|else";
        let stripped = strip_line_number_prefix(s).unwrap();
        assert_eq!(stripped, "something\nelse");
    }

    #[test]
    fn desanitize_replaces_curly_quotes_and_nbsp() {
        let s = "x \u{201C}y\u{201D} z\u{00A0}w";
        assert_eq!(desanitize_text(s), "x \"y\" z w");
    }
}
