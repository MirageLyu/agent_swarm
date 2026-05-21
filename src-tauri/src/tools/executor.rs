use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};
use tauri::Emitter;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolOutput {
    pub content: String,
    pub is_error: bool,
}

impl ToolOutput {
    fn ok(content: String) -> Self {
        Self {
            content,
            is_error: false,
        }
    }

    fn error(kind: &str, message: &str) -> Self {
        Self {
            content: serde_json::json!({ "error": kind, "message": message }).to_string(),
            is_error: true,
        }
    }
}

pub struct ToolExecutor {
    workspace_root: PathBuf,
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
        let workspace_root = workspace_root
            .canonicalize()
            .unwrap_or(workspace_root);
        Self {
            workspace_root,
            read_paths: Arc::new(Mutex::new(HashMap::new())),
            cancel_token: None,
        }
    }

    /// Builder：注入 cancel_token，让 shell_exec 等长跑工具能响应用户取消。
    /// AgentEngine 在创建 ToolExecutor 后立刻调一次。
    pub fn with_cancel_token(
        mut self,
        token: tokio_util::sync::CancellationToken,
    ) -> Self {
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
            "shell_exec" => self.shell_exec(input, None).await,
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
        if tool_name == "shell_exec" {
            self.shell_exec(input, Some(StreamCtx {
                app: app.clone(),
                agent_id: agent_id.to_string(),
            }))
            .await
        } else {
            self.execute(tool_name, input).await
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
            None => return ToolOutput::error(
                "parameter_error",
                &format!("Missing 'path' parameter. Received: {input}"),
            ),
        };
        let offset = input.get("offset").and_then(|v| v.as_u64());
        let limit = input.get("limit").and_then(|v| v.as_u64());
        let is_paged_request = offset.is_some() || limit.is_some();

        let full_path = match self.resolve_path(rel_path) {
            Ok(p) => p,
            Err(e) => return ToolOutput::error("sandbox_violation", &e.to_string()),
        };
        let canonical = full_path.canonicalize().unwrap_or_else(|_| full_path.clone());

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
        let start_idx: usize = match offset {
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
        let end_idx = (start_idx + requested_limit).min(total_lines);
        let truncated_head = start_idx > 0;
        let truncated_tail = end_idx < total_lines;

        let mut rendered = String::with_capacity(content.len() + total_lines * 8);
        if truncated_head {
            rendered.push_str(&format!(
                "[skipped lines 1..{}; pass offset=1 to start from top]\n",
                start_idx
            ));
        }
        for (i, line) in lines[start_idx..end_idx].iter().enumerate() {
            let line_num = start_idx + i + 1;
            // 6 位右对齐 + `|` 分隔；和 inline_line_numbers 系统提示对齐。
            rendered.push_str(&format!("{:>6}|{}\n", line_num, line));
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
            self.read_paths.lock().unwrap().insert(canonical, current_mtime);
        } else {
            // 分页时也至少登记"看过"，让 edit_file 不再卡 read precondition（但 mtime 用占位值
            // 表示"未必最新"，下次整体读会失效 stub）。
            self.read_paths
                .lock()
                .unwrap()
                .entry(canonical)
                .or_insert(SystemTime::UNIX_EPOCH);
        }
        ToolOutput::ok(rendered)
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
            None => return ToolOutput::error(
                "parameter_error",
                &format!("Missing 'path' parameter. Received keys: {:?}",
                    input.as_object().map(|o| o.keys().collect::<Vec<_>>())),
            ),
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
                    None => return ToolOutput::error(
                        "parameter_error",
                        &format!("edits[{i}] missing 'old_string'."),
                    ),
                };
                let new_s = match item.get("new_string").and_then(|v| v.as_str()) {
                    Some(s) => s.to_string(),
                    None => return ToolOutput::error(
                        "parameter_error",
                        &format!("edits[{i}] missing 'new_string' (pass empty string to delete)."),
                    ),
                };
                let ra = item.get("replace_all").and_then(|v| v.as_bool()).unwrap_or(false);
                edits.push(EditOp { old_string: old_s, new_string: new_s, replace_all: ra });
            }
        } else {
            let old_s = match input["old_string"].as_str() {
                Some(s) => s.to_string(),
                None => return ToolOutput::error(
                    "parameter_error",
                    "Missing 'old_string' parameter (or pass an `edits` array for multi-edit).",
                ),
            };
            let new_s = match input["new_string"].as_str() {
                Some(s) => s.to_string(),
                None => return ToolOutput::error(
                    "parameter_error",
                    "Missing 'new_string' parameter (pass empty string to delete).",
                ),
            };
            let ra = input.get("replace_all").and_then(|v| v.as_bool()).unwrap_or(false);
            edits.push(EditOp { old_string: old_s, new_string: new_s, replace_all: ra });
        }

        let full_path = match self.resolve_path(rel_path) {
            Ok(p) => p,
            Err(e) => return ToolOutput::error("sandbox_violation", &e.to_string()),
        };
        let canonical = full_path.canonicalize().unwrap_or_else(|_| full_path.clone());

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
        let rel_path = match input["path"].as_str() {
            Some(p) => p,
            None => return ToolOutput::error(
                "parameter_error",
                &format!("Missing 'path' parameter. Received: {input}"),
            ),
        };
        let content = match input["content"].as_str() {
            Some(c) => c,
            None => return ToolOutput::error(
                "parameter_error",
                &format!("Missing 'content' parameter. Received keys: {:?}", input.as_object().map(|o| o.keys().collect::<Vec<_>>())),
            ),
        };
        let full_path = match self.resolve_path(rel_path) {
            Ok(p) => p,
            Err(e) => return ToolOutput::error("sandbox_violation", &e.to_string()),
        };

        if let Some(parent) = full_path.parent() {
            if let Err(e) = tokio::fs::create_dir_all(parent).await {
                return ToolOutput::error("io_error", &e.to_string());
            }
        }
        match tokio::fs::write(&full_path, content).await {
            Ok(()) => {
                // 关键修复：写完后必须把 canonical 路径登记进 read_paths，否则
                // agent 接下来想 edit_file 这个文件会被 edit_without_read 拒绝，
                // 强迫它再读一遍刚刚自己写的文件——纯浪费 token。
                let canonical = full_path.canonicalize().unwrap_or(full_path);
                self.record_path_mtime(canonical);
                ToolOutput::ok(format!("Written {} bytes to {rel_path}", content.len()))
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
            None => return ToolOutput::error(
                "parameter_error",
                "Missing 'pattern' parameter (e.g. `**/*.rs`).",
            ),
        };
        let base_rel = input["path"].as_str().unwrap_or(".");
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
            None => return ToolOutput::error(
                "parameter_error",
                "Pattern path contains non-UTF8 bytes",
            ),
        };

        let walk = match glob::glob(pattern_str) {
            Ok(it) => it,
            Err(e) => return ToolOutput::error(
                "parameter_error",
                &format!("invalid glob pattern: {e}"),
            ),
        };

        let workspace_canonical = self
            .workspace_root
            .canonicalize()
            .unwrap_or_else(|_| self.workspace_root.clone());

        let mut entries: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
        for path in walk.flatten() {
            // sandbox 兜底：glob 不会自己越界，但符号链接可能；显式校验。
            let canonical = path.canonicalize().unwrap_or_else(|_| path.clone());
            if !canonical.starts_with(&workspace_canonical) {
                continue;
            }
            // 只列文件，不列目录——和 GlobTool 语义一致；LLM 列目录用 list_files。
            let mtime = match tokio::fs::metadata(&path).await {
                Ok(meta) if meta.is_file() => meta
                    .modified()
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH),
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
            .map(|(_, path)| {
                path.strip_prefix(&workspace_canonical)
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|_| path.display().to_string())
            })
            .collect();
        if truncated {
            lines.push(format!("... ({} matches; showing newest {})", total, limit));
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
            None => return ToolOutput::error(
                "parameter_error",
                &format!("Missing 'pattern' parameter. Received: {input}"),
            ),
        };
        let search_path = match input["path"].as_str() {
            Some(p) => match self.resolve_path(p) {
                Ok(path) => path,
                Err(e) => return ToolOutput::error("sandbox_violation", &e.to_string()),
            },
            None => self.workspace_root.clone(),
        };

        let glob_pat = input.get("glob").and_then(|v| v.as_str());
        let type_filter = input.get("type").and_then(|v| v.as_str());
        let case_insensitive = input
            .get("case_insensitive")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let multiline = input.get("multiline").and_then(|v| v.as_bool()).unwrap_or(false);
        let context_before = input.get("context_before").and_then(|v| v.as_u64());
        let context_after = input.get("context_after").and_then(|v| v.as_u64());
        let context = input.get("context").and_then(|v| v.as_u64());
        let head_limit = input
            .get("head_limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(200) as usize;
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
        // 始终 color=never，避免前端拿到 ANSI 控制字符。
        args.push("--color=never".into());
        // 用 -e 把 pattern 当字面参数传，避免 pattern 以 `-` 开头被当 flag。
        args.push("-e".into());
        args.push(pattern.into());

        let output = match Command::new("rg")
            .args(&args)
            .current_dir(&search_path)
            .output()
            .await
        {
            Ok(o) => o,
            Err(e) => return ToolOutput::error("io_error", &format!("failed to spawn rg: {e}")),
        };

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let exit_code = output.status.code().unwrap_or(-1);

        // rg 退出码：0=有匹配 1=无匹配 2+=真正的错误。
        if exit_code == 1 {
            return ToolOutput::ok(format!(
                "No matches for pattern `{pattern}`{}.",
                glob_pat.map(|g| format!(" (glob `{g}`)")).unwrap_or_default()
            ));
        }
        if exit_code >= 2 {
            return ToolOutput::error(
                "rg_error",
                &format!("ripgrep exited with code {exit_code}: {}", stderr.trim()),
            );
        }

        // head_limit 截断（按行数）。
        let lines: Vec<&str> = stdout.lines().collect();
        let total = lines.len();
        let truncated = total > head_limit;
        let kept = if truncated { &lines[..head_limit] } else { &lines[..] };
        let mut body = kept.join("\n");
        if truncated {
            body.push_str(&format!(
                "\n... [truncated {} more lines; pass head_limit higher or narrow with glob/type]",
                total - head_limit
            ));
        }
        if body.is_empty() {
            body = format!("(rg returned no output for `{pattern}`)");
        }
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
    ) -> ToolOutput {
        let command = match input["command"].as_str() {
            Some(c) => c,
            None => return ToolOutput::error(
                "parameter_error",
                &format!("Missing 'command' parameter. Received: {input}"),
            ),
        };
        let expect_long_running = input
            .get("expect_long_running")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let (idle_secs, wall_secs) = if expect_long_running {
            (SHELL_LONG_IDLE_SECS, SHELL_LONG_WALL_SECS)
        } else {
            (SHELL_DEFAULT_IDLE_SECS, SHELL_DEFAULT_WALL_SECS)
        };

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

        let mut child = match Command::new("sh")
            .args(["-c", command])
            .current_dir(&self.workspace_root)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
        {
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
                return ToolOutput::error("shell_error", &e.to_string());
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

        let last_byte_at = Arc::new(Mutex::new(Instant::now()));
        let stdout_buf = Arc::new(Mutex::new(TruncatedBuffer::new(SHELL_OUTPUT_MAX_BYTES)));
        let stderr_buf = Arc::new(Mutex::new(TruncatedBuffer::new(SHELL_OUTPUT_MAX_BYTES)));

        let stdout_handle = stdout_pipe.map(|p| {
            spawn_pipe_reader(
                p,
                stdout_buf.clone(),
                last_byte_at.clone(),
                stream_ctx.clone().map(|c| (c, "stdout".to_string())),
            )
        });
        let stderr_handle = stderr_pipe.map(|p| {
            spawn_pipe_reader(
                p,
                stderr_buf.clone(),
                last_byte_at.clone(),
                stream_ctx.clone().map(|c| (c, "stderr".to_string())),
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
        let finalize =
            |status: std::io::Result<std::process::ExitStatus>,
             termination_reason: Option<String>,
             stdout_text: String,
             stderr_text: String,
             elapsed: std::time::Duration| {
                match status {
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
                        let msg = format!(
                            "Command failed (exit code {code}, elapsed {:.1}s)\n\
                             [stdout]\n{stdout_text}\n[stderr]\n{stderr_text}",
                            elapsed.as_secs_f64()
                        );
                        ToolOutput::error("shell_error", &msg)
                    }
                    Err(e) => ToolOutput::error("shell_error", &e.to_string()),
                }
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

                    if elapsed.as_secs() > wall_secs {
                        termination_reason = Some(format!(
                            "wall_clock {wall_secs}s exceeded (elapsed {:.1}s)",
                            elapsed.as_secs_f64()
                        ));
                    } else if idle.as_secs() > idle_secs {
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
        }
    }

    async fn list_files(&self, input: &serde_json::Value) -> ToolOutput {
        let rel_path = input["path"].as_str().unwrap_or(".");
        let full_path = match self.resolve_path(rel_path) {
            Ok(p) => p,
            Err(e) => return ToolOutput::error("sandbox_violation", &e.to_string()),
        };

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
                    let suffix = if file_type.is_dir() { "/" } else { "" };
                    entries.push(format!("{name}{suffix}"));
                }
                Ok(None) => break,
                Err(e) => return ToolOutput::error("io_error", &e.to_string()),
            }
        }
        entries.sort();
        ToolOutput::ok(entries.join("\n"))
    }

    fn resolve_path(&self, rel_path: &str) -> Result<PathBuf> {
        let full = self.workspace_root.join(rel_path);
        let canonical = full
            .canonicalize()
            .unwrap_or_else(|_| Self::normalize_lexical(&full));

        let workspace_canonical = self
            .workspace_root
            .canonicalize()
            .unwrap_or_else(|_| Self::normalize_lexical(&self.workspace_root));

        if !canonical.starts_with(&workspace_canonical) {
            bail!(
                "Path escapes workspace: {} is outside {}",
                canonical.display(),
                workspace_canonical.display()
            );
        }
        Ok(full)
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

/// edit_file 内部错误。`kind` 直接喂 ToolOutput::error，message 拼回 LLM。
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
        let stripped_new = strip_line_number_prefix(new_string).unwrap_or_else(|| new_string.to_string());
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

/// 限长的字节缓冲：超过 `cap` 时丢弃头部、保留尾部，并标记 `truncated_head_bytes`。
struct TruncatedBuffer {
    cap: usize,
    inner: Vec<u8>,
    truncated_head_bytes: usize,
}

impl TruncatedBuffer {
    fn new(cap: usize) -> Self {
        Self { cap, inner: Vec::new(), truncated_head_bytes: 0 }
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
    async fn shell_success_returns_stdout() {
        let (_dir, exec) = setup();
        let out = exec
            .execute(
                "shell_exec",
                &serde_json::json!({"command": "echo hello"}),
            )
            .await;
        assert!(!out.is_error);
        assert!(out.content.contains("hello"));
    }

    #[tokio::test]
    async fn shell_failure_returns_structured_error() {
        let (_dir, exec) = setup();
        let out = exec
            .execute(
                "shell_exec",
                &serde_json::json!({"command": "exit 1"}),
            )
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
        assert!(v["message"].as_str().unwrap().contains("127"));
    }

    #[tokio::test]
    async fn read_file_not_found() {
        let (_dir, exec) = setup();
        let out = exec
            .execute(
                "read_file",
                &serde_json::json!({"path": "nonexistent.txt"}),
            )
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

    // ---- Single-Agent Uplift Phase 1.1: edit_file 单测 ----

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
        assert_eq!(fs::read_to_string(dir.path().join("a.txt")).unwrap(), "hello world");
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
        assert_eq!(fs::read_to_string(dir.path().join("a.txt")).unwrap(), "hello rust");
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
        assert_eq!(fs::read_to_string(dir.path().join("b.txt")).unwrap(), "x x x");
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
        assert_eq!(fs::read_to_string(dir.path().join("c.txt")).unwrap(), "bar bar bar");
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
        assert_eq!(fs::read_to_string(dir.path().join("e.txt")).unwrap(), "content");
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
        assert!(out.content.contains("     1|alpha"), "got:\n{}", out.content);
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
        assert!(out.content.contains("    10|line10"), "got:\n{}", out.content);
        assert!(out.content.contains("    11|line11"));
        assert!(out.content.contains("    12|line12"));
        assert!(!out.content.contains("    13|line13"), "should not include past limit");
        // 应当告知有截断（end_idx=12 < 50）
        assert!(out.content.contains("truncated at line 12 of 50"));
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
        assert!(!out_e.is_error, "edit_file should accept just-written file: {}", out_e.content);
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
        assert_eq!(fs::read_to_string(dir.path().join("m.txt")).unwrap(), "ALPHA beta GAMMA");
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
        fs::write(dir.path().join("ln.txt"), "fn foo() {\n    println!(\"hi\");\n}\n").unwrap();
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

    // ---- search_files 升级 ----
    // 注意：search_files 走外部 `rg`。如果当前机器没装 ripgrep，跳过——CI/dev 机器需要装。

    fn rg_available() -> bool {
        std::process::Command::new("rg")
            .arg("--version")
            .output()
            .is_ok()
    }

    #[tokio::test]
    async fn search_files_files_with_matches_mode() {
        if !rg_available() {
            eprintln!("[skip] ripgrep (`rg`) not on PATH; skipping search_files tests");
            return;
        }
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
        if !rg_available() {
            return;
        }
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
        if !rg_available() {
            return;
        }
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
        if !rg_available() {
            eprintln!("[skip] ripgrep (`rg`) not on PATH; skipping grep tests");
            return;
        }
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
        if !rg_available() {
            return;
        }
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

    /// 关键回归：通过主名 `grep` 调与通过 alias `search_files` 调，行为必须**字节相等**。
    /// 否则 alias 兼容性是假的。
    #[tokio::test]
    async fn grep_and_search_files_alias_produce_identical_output() {
        if !rg_available() {
            return;
        }
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
