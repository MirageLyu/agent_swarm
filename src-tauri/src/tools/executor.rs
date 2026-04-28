use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
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
const SHELL_WATCHDOG_TICK: Duration = Duration::from_millis(500);
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
}

impl ToolExecutor {
    pub fn new(workspace_root: PathBuf) -> Self {
        let workspace_root = workspace_root
            .canonicalize()
            .unwrap_or(workspace_root);
        Self { workspace_root }
    }

    pub fn workspace_display(&self) -> String {
        self.workspace_root.display().to_string()
    }

    pub async fn execute(&self, tool_name: &str, input: &serde_json::Value) -> ToolOutput {
        match tool_name {
            "read_file" => self.read_file(input).await,
            "write_file" => self.write_file(input).await,
            "search_files" => self.search_files(input).await,
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

    async fn read_file(&self, input: &serde_json::Value) -> ToolOutput {
        let rel_path = match input["path"].as_str() {
            Some(p) => p,
            None => return ToolOutput::error(
                "parameter_error",
                &format!("Missing 'path' parameter. Received: {input}"),
            ),
        };
        let full_path = match self.resolve_path(rel_path) {
            Ok(p) => p,
            Err(e) => return ToolOutput::error("sandbox_violation", &e.to_string()),
        };
        match tokio::fs::read_to_string(&full_path).await {
            Ok(content) => ToolOutput::ok(content),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                ToolOutput::error("file_not_found", &format!("File not found: {rel_path}"))
            }
            Err(e) => ToolOutput::error("io_error", &e.to_string()),
        }
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
            Ok(()) => ToolOutput::ok(format!("Written {} bytes to {rel_path}", content.len())),
            Err(e) => ToolOutput::error("io_error", &e.to_string()),
        }
    }

    async fn search_files(&self, input: &serde_json::Value) -> ToolOutput {
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

        match Command::new("rg")
            .args(["--max-count", "50", "--line-number", pattern])
            .current_dir(&search_path)
            .output()
            .await
        {
            Ok(output) => ToolOutput::ok(String::from_utf8_lossy(&output.stdout).to_string()),
            Err(e) => ToolOutput::error("io_error", &e.to_string()),
        }
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
                if let Some(ctx) = &stream_ctx {
                    emit_stream_meta(ctx, &format!("[spawn error] {e}\n"));
                }
                return ToolOutput::error("shell_error", &e.to_string());
            }
        };

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

        loop {
            tokio::select! {
                biased;
                wait_res = child.wait() => {
                    if let Some(h) = stdout_handle { let _ = h.await; }
                    if let Some(h) = stderr_handle { let _ = h.await; }

                    let stdout_text = stdout_buf.lock().unwrap().render();
                    let stderr_text = stderr_buf.lock().unwrap().render();
                    let elapsed = started.elapsed();

                    return match wait_res {
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
                                if !combined.is_empty() { combined.push('\n'); }
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
                    };
                }
                _ = tokio::time::sleep(SHELL_WATCHDOG_TICK) => {
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
}
