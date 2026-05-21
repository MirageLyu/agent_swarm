//! Single-Agent Uplift P2-1 Phase C：CommandHook —— 把外部脚本接入 hook 体系。
//!
//! # 协议
//!
//! ## stdin（hook 进程读）
//!
//! `HookContext` 的 JSON 序列化形式（schema 跟 [`crate::agent::hooks::HookContext`] 一致）。
//! 例：
//! ```json
//! {
//!   "agent_id": "agent-123",
//!   "mission_id": "mission-456",
//!   "workspace_path": "/Users/me/repo",
//!   "step": 12,
//!   "phase": "PostToolUse",
//!   "messages_summary": { "total_count": 18, "last_assistant_text": "...", "recent_tool_uses": ["write_file","read_file"] },
//!   "last_tool_use": { "tool_use_id": "...", "tool_name": "write_file", "input": {...}, "output_excerpt": "...", "is_error": false },
//!   "task_complete_summary": null
//! }
//! ```
//!
//! ## stdout（hook 进程写）
//!
//! [`HookOutcome`] 的 JSON。例：
//! ```json
//! "Pass"
//! ```
//! 或：
//! ```json
//! { "InjectMessage": { "content": "tsc reported 3 errors:\n...", "severity": "Warning" } }
//! ```
//! 或：
//! ```json
//! { "PreventContinuation": { "reason": "lint fatal", "terminal": true } }
//! ```
//!
//! ## exit code
//!
//! - **0**：使用 stdout 的 outcome；stdout 解析失败 → 退化为 Pass + warn 日志
//! - **非 0**：自动转 `InjectMessage { severity: Warning, content: stderr 截前 2KB }`
//!   语义："hook 失败了，请 agent 看一下 stderr 决定怎么办"——而不是直接 fail
//!   整个 agent（命令 hook 不该有"杀 mission"的权力，除非显式 PreventContinuation）
//!
//! # 安全模型
//!
//! 见 [`crate::agent::hooks::config`] 的安全 doc。核心：
//! - **默认禁用**：`AppConfig.allow_command_hooks=false`
//! - **仅 workspace 内 `.miragenty/hooks.json`**：不接受 user-global 路径
//! - **每个 hook 60s timeout**：防恶意阻塞 agent loop
//! - **stdout 解析失败 = Pass**：避免 hook 写错让 agent 整个 fail
//!
//! # 为什么 timeout 60s
//!
//! 典型用例（tsc / lint / npm test）在中型项目 30s 内完成；60s 让较慢 tsc 配置
//! 也能跑过。更长会让 agent 单 step 在 hook 上等太久——hook 应该是"轻量 sanity check"，
//! 不是"完整 CI"。如果用户真要跑长任务，应在 Stop phase 而非 PostToolUse。

use crate::agent::hooks::{AgentHook, HookContext, HookOutcome, HookPhase, HookSeverity};
use serde::Deserialize;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::time::timeout;

/// 单条 hook 命令的执行超时。详见模块文档"为什么 60s"。
const COMMAND_HOOK_TIMEOUT: Duration = Duration::from_secs(60);

/// stderr 截尾上限（bytes）：防 hook 吐 100MB error spam 进 conversation。
const STDERR_EXCERPT_BYTES: usize = 2048;

/// stdout 解析窗口（bytes）：JSON outcome 极短（< 1KB），多读浪费。
const STDOUT_PARSE_LIMIT_BYTES: usize = 65_536;

/// CommandHook：用户通过 `.miragenty/hooks.json` 注册的外部命令 hook。
///
/// **不要直接构造**——必须经 [`crate::agent::hooks::config::load_workspace_hooks`]，
/// 那里会校验 `allow_command_hooks` flag 和 workspace 路径。
#[derive(Debug, Clone)]
pub struct CommandHook {
    /// `"command:<shell snippet>"` —— `command:` 前缀让 hook_executed 事件能区分
    /// 是 builtin 还是 command 来源。
    name: String,
    phases: Vec<HookPhase>,
    /// 可选 matcher：正则 / 工具名匹配。空 = 全 match。
    /// 当前最小实现：字符串包含 ctx.last_tool_use.tool_name。
    /// 后续可扩展正则。
    matcher: Option<String>,
    /// 执行命令。**完整 shell snippet**：会被 `sh -c <command>` 包起来。
    /// 这给用户最大灵活性（管道 / 条件 / 多命令），代价是 escape 责任在用户。
    command: String,
    /// 命令工作目录（默认 workspace_path）
    working_dir: PathBuf,
}

impl CommandHook {
    /// 构造一个 CommandHook。`workspace_path` 用作 cwd。
    pub fn new(
        name: String,
        phases: Vec<HookPhase>,
        command: String,
        matcher: Option<String>,
        workspace_path: PathBuf,
    ) -> Self {
        Self {
            name,
            phases,
            matcher,
            command,
            working_dir: workspace_path,
        }
    }
}

/// 内部辅助：把 ctx 序列化后通过 stdin 喂给 sh -c。
async fn run_command_with_stdin(
    command: &str,
    working_dir: &std::path::Path,
    stdin_payload: &str,
) -> std::io::Result<std::process::Output> {
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(working_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // 显式不继承父进程 env 之外的特殊变量；当前继承 PATH 等是必要的
        // （hook 命令依赖 PATH 找到 tsc / npm 等）。
        .spawn()?;

    if let Some(mut stdin) = child.stdin.take() {
        // best-effort 写 stdin；失败不致命，hook 可能不读 stdin
        let _ = stdin.write_all(stdin_payload.as_bytes()).await;
        let _ = stdin.shutdown().await;
    }

    child.wait_with_output().await
}

#[async_trait::async_trait]
impl AgentHook for CommandHook {
    fn name(&self) -> &str {
        &self.name
    }

    fn phases(&self) -> &[HookPhase] {
        &self.phases
    }

    fn matches(&self, ctx: &HookContext) -> bool {
        let m = match &self.matcher {
            Some(s) if !s.trim().is_empty() => s.trim(),
            _ => return true,
        };
        // 子串匹配：tool 名包含 matcher，或 phase 名匹配 matcher。
        //
        // 双向 canonicalize：用户写 `matcher: "search_files"` 时，新 grep 事件应仍触发；
        // 反之用户写 `matcher: "grep"` 时也能命中历史 search_files 事件（如果有）。
        // 同时保留原始字符串比对，避免宽匹配（如 `matcher: "file"`）因 canonicalize
        // 改变行为。详见 `tools::registry::canonicalize`。
        if let Some(tool) = &ctx.last_tool_use {
            let raw_name = tool.tool_name.as_str();
            let canon_name = crate::tools::canonicalize_tool_name(raw_name);
            let canon_m = crate::tools::canonicalize_tool_name(m);
            if raw_name.contains(m)
                || canon_name.contains(canon_m)
                || raw_name.contains(canon_m)
                || canon_name.contains(m)
            {
                return true;
            }
        }
        ctx.phase.as_str().eq_ignore_ascii_case(m)
    }

    async fn execute(&self, ctx: &HookContext) -> HookOutcome {
        let stdin_payload = match serde_json::to_string(ctx) {
            Ok(s) => s,
            Err(e) => {
                // 自身序列化失败应该极少（HookContext 字段都是 String/数字/Value）；
                // 万一发生不让 hook block 主流程，直接 warn + Pass。
                tracing::warn!(
                    hook = %self.name,
                    "CommandHook stdin payload serialization failed: {e}"
                );
                return HookOutcome::Pass;
            }
        };

        let run = run_command_with_stdin(&self.command, &self.working_dir, &stdin_payload);
        let output = match timeout(COMMAND_HOOK_TIMEOUT, run).await {
            Ok(Ok(out)) => out,
            Ok(Err(e)) => {
                tracing::warn!(
                    hook = %self.name,
                    "CommandHook spawn failed: {e}"
                );
                // spawn 失败（如 sh 不存在）转 Inject 让 agent 看到错误而非默默忽略
                return HookOutcome::InjectMessage {
                    content: format!(
                        "Command hook `{}` failed to spawn: {e}. Treat as advisory.",
                        self.name
                    ),
                    severity: HookSeverity::Warning,
                };
            }
            Err(_elapsed) => {
                tracing::warn!(
                    hook = %self.name,
                    timeout_secs = COMMAND_HOOK_TIMEOUT.as_secs(),
                    "CommandHook exceeded timeout"
                );
                return HookOutcome::InjectMessage {
                    content: format!(
                        "Command hook `{}` exceeded {}s timeout and was killed.",
                        self.name,
                        COMMAND_HOOK_TIMEOUT.as_secs()
                    ),
                    severity: HookSeverity::Warning,
                };
            }
        };

        let stdout_text = String::from_utf8_lossy(
            &output.stdout[..output.stdout.len().min(STDOUT_PARSE_LIMIT_BYTES)],
        )
        .to_string();
        let stderr_text = String::from_utf8_lossy(
            &output.stderr[..output.stderr.len().min(STDERR_EXCERPT_BYTES)],
        )
        .to_string();

        if !output.status.success() {
            // 非 0 退出：当作"hook 失败" → InjectMessage(warning) 让 agent 知情
            return HookOutcome::InjectMessage {
                content: format!(
                    "Command hook `{}` exited {}.\nstderr (first {} bytes):\n{}",
                    self.name,
                    output
                        .status
                        .code()
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "signal".into()),
                    STDERR_EXCERPT_BYTES,
                    stderr_text.trim()
                ),
                severity: HookSeverity::Warning,
            };
        }

        // 解析 stdout 为 HookOutcome；解析失败退化为 Pass（**不要**让 hook 配错就 fail agent）
        let trimmed = stdout_text.trim();
        if trimmed.is_empty() {
            return HookOutcome::Pass;
        }
        match serde_json::from_str::<HookOutcome>(trimmed) {
            Ok(outcome) => outcome,
            Err(e) => {
                tracing::warn!(
                    hook = %self.name,
                    "CommandHook stdout JSON parse failed: {e}; raw={}",
                    trimmed.chars().take(200).collect::<String>()
                );
                HookOutcome::Pass
            }
        }
    }
}

/// 给配置加载层用的 raw schema。从 `hooks.json` 反序列化后由 loader 转成 [`CommandHook`]。
#[derive(Debug, Clone, Deserialize)]
pub struct CommandHookSpec {
    /// 给 timeline 显示用。`namespace/name` 风格，如 `workspace/lint-after-write`。
    /// 自动加 `command:` 前缀。
    pub name: String,
    /// 哪些 phase 触发。字符串与 [`HookPhase::from_str`] 一致。
    pub phases: Vec<String>,
    /// 可选 matcher：当前最小实现是子串匹配工具名 / phase 名。
    #[serde(default)]
    pub matcher: Option<String>,
    /// shell snippet。会被 `sh -c <command>` 跑。
    pub command: String,
}

#[cfg(test)]
mod tests {
    //! CommandHook 的单测重点：
    //!   1. exit 0 + 合法 JSON stdout → 对应 HookOutcome
    //!   2. exit 0 + 空 stdout → Pass
    //!   3. exit 非 0 → Inject(warning) 带 stderr 截尾
    //!   4. timeout → Inject(warning) 带 timeout 文案
    //!   5. matcher 子串匹配工具名
    //!   6. CommandHookSpec JSON 反序列化稳定

    use super::*;
    use crate::agent::hooks::{HookMessagesSummary, HookToolUseInfo};

    fn mock_ctx(phase: HookPhase, tool_name: Option<&str>) -> HookContext {
        HookContext {
            agent_id: "a".into(),
            mission_id: "m".into(),
            workspace_path: "/tmp".into(),
            step: 1,
            phase,
            messages_summary: HookMessagesSummary {
                total_count: 0,
                last_assistant_text: None,
                recent_tool_uses: vec![],
            },
            last_tool_use: tool_name.map(|n| HookToolUseInfo {
                tool_use_id: "u1".into(),
                tool_name: n.into(),
                input: serde_json::json!({}),
                output_excerpt: String::new(),
                is_error: false,
            }),
            task_complete_summary: None,
        }
    }

    #[tokio::test]
    async fn exit_zero_with_pass_stdout_returns_pass() {
        let hook = CommandHook::new(
            "command:test/echo-pass".into(),
            vec![HookPhase::Stop],
            r#"printf '"Pass"'"#.into(),
            None,
            std::env::temp_dir(),
        );
        let out = hook.execute(&mock_ctx(HookPhase::Stop, None)).await;
        assert!(matches!(out, HookOutcome::Pass));
    }

    #[tokio::test]
    async fn exit_zero_with_inject_json_returns_inject() {
        let hook = CommandHook::new(
            "command:test/echo-inject".into(),
            vec![HookPhase::Stop],
            // 注意 shell 内 single-quote 转义不能直接嵌套，所以用 printf 的 %s 喂 JSON
            r#"printf '%s' '{"InjectMessage":{"content":"hi","severity":"Warning"}}'"#.into(),
            None,
            std::env::temp_dir(),
        );
        let out = hook.execute(&mock_ctx(HookPhase::Stop, None)).await;
        match out {
            HookOutcome::InjectMessage { content, severity } => {
                assert_eq!(content, "hi");
                assert!(matches!(severity, HookSeverity::Warning));
            }
            other => panic!("expected Inject, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn exit_zero_with_empty_stdout_returns_pass() {
        let hook = CommandHook::new(
            "command:test/silent".into(),
            vec![HookPhase::Stop],
            "true".into(),
            None,
            std::env::temp_dir(),
        );
        let out = hook.execute(&mock_ctx(HookPhase::Stop, None)).await;
        assert!(matches!(out, HookOutcome::Pass));
    }

    #[tokio::test]
    async fn non_zero_exit_returns_inject_warning_with_stderr() {
        let hook = CommandHook::new(
            "command:test/fail".into(),
            vec![HookPhase::Stop],
            "echo oops >&2; exit 7".into(),
            None,
            std::env::temp_dir(),
        );
        let out = hook.execute(&mock_ctx(HookPhase::Stop, None)).await;
        match out {
            HookOutcome::InjectMessage { content, severity } => {
                assert!(matches!(severity, HookSeverity::Warning));
                assert!(content.contains("exited 7"));
                assert!(content.contains("oops"), "stderr 应被嵌入: {content}");
            }
            other => panic!("expected Inject(warning), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn malformed_stdout_returns_pass_with_warn() {
        let hook = CommandHook::new(
            "command:test/bad-json".into(),
            vec![HookPhase::Stop],
            "echo 'this is not JSON'".into(),
            None,
            std::env::temp_dir(),
        );
        let out = hook.execute(&mock_ctx(HookPhase::Stop, None)).await;
        // 关键不变量：hook 配错了，不让它把 agent 搞 fail
        assert!(matches!(out, HookOutcome::Pass));
    }

    #[tokio::test]
    async fn matcher_filters_by_tool_name_substring() {
        let hook = CommandHook::new(
            "command:test/match-write".into(),
            vec![HookPhase::PostToolUse],
            "true".into(),
            Some("write".into()),
            std::env::temp_dir(),
        );
        // tool_name 包含 "write" → 匹配
        assert!(hook.matches(&mock_ctx(HookPhase::PostToolUse, Some("write_file"))));
        assert!(hook.matches(&mock_ctx(HookPhase::PostToolUse, Some("edit_write"))));
        // 不包含 → 不匹配
        assert!(!hook.matches(&mock_ctx(HookPhase::PostToolUse, Some("read_file"))));
        // 无 tool 但 phase 名也不匹配
        assert!(!hook.matches(&mock_ctx(HookPhase::PostToolUse, None)));
    }

    #[tokio::test]
    async fn no_matcher_matches_all() {
        let hook = CommandHook::new(
            "command:test/always".into(),
            vec![HookPhase::Stop],
            "true".into(),
            None,
            std::env::temp_dir(),
        );
        assert!(hook.matches(&mock_ctx(HookPhase::Stop, None)));
        assert!(hook.matches(&mock_ctx(HookPhase::Stop, Some("anything"))));
    }

    /// 关键回归（2026-05 grep 重命名）：用户写过 `matcher: "search_files"` 的旧 hook config
    /// 必须仍然在新 `grep` 事件上触发——这是改名"零行为变更"承诺的核心。
    #[tokio::test]
    async fn matcher_search_files_matches_new_grep_event() {
        let hook = CommandHook::new(
            "command:test/legacy-search-files-matcher".into(),
            vec![HookPhase::PostToolUse],
            "true".into(),
            Some("search_files".into()),
            std::env::temp_dir(),
        );
        // 新 LLM 发出的事件名是 grep；旧 matcher 仍要命中
        assert!(
            hook.matches(&mock_ctx(HookPhase::PostToolUse, Some("grep"))),
            "legacy matcher `search_files` must match new `grep` events"
        );
        // 兜底：仍能匹配原 search_files alias 事件（如果有旧 replay）
        assert!(hook.matches(&mock_ctx(HookPhase::PostToolUse, Some("search_files"))));
    }

    /// 反方向：用户用新 `matcher: "grep"` 配置 hook，遇到历史 replay 的 `search_files`
    /// 事件也应该匹配——否则查问题时旧 timeline 上 hook 全部缺席，难调试。
    #[tokio::test]
    async fn matcher_grep_matches_legacy_search_files_event() {
        let hook = CommandHook::new(
            "command:test/new-grep-matcher".into(),
            vec![HookPhase::PostToolUse],
            "true".into(),
            Some("grep".into()),
            std::env::temp_dir(),
        );
        assert!(
            hook.matches(&mock_ctx(HookPhase::PostToolUse, Some("search_files"))),
            "new matcher `grep` should also match legacy `search_files` events"
        );
        assert!(hook.matches(&mock_ctx(HookPhase::PostToolUse, Some("grep"))));
    }

    /// 边界：宽匹配（如 `matcher: "file"`）不应因 canonicalize 改变语义——
    /// `file` 不在 alias 表里，仍走原始子串匹配。
    #[tokio::test]
    async fn matcher_canonicalize_does_not_break_wide_substring_match() {
        let hook = CommandHook::new(
            "command:test/wide-file".into(),
            vec![HookPhase::PostToolUse],
            "true".into(),
            Some("file".into()),
            std::env::temp_dir(),
        );
        assert!(hook.matches(&mock_ctx(HookPhase::PostToolUse, Some("read_file"))));
        assert!(hook.matches(&mock_ctx(HookPhase::PostToolUse, Some("write_file"))));
        assert!(hook.matches(&mock_ctx(HookPhase::PostToolUse, Some("list_files"))));
        // grep 不含 "file" 子串
        assert!(!hook.matches(&mock_ctx(HookPhase::PostToolUse, Some("grep"))));
    }

    #[test]
    fn command_hook_spec_roundtrip_json() {
        let raw = r#"{
            "name": "workspace/lint",
            "phases": ["PostToolUse","Stop"],
            "matcher": "write_file",
            "command": "npm run lint"
        }"#;
        let spec: CommandHookSpec = serde_json::from_str(raw).unwrap();
        assert_eq!(spec.name, "workspace/lint");
        assert_eq!(spec.phases, vec!["PostToolUse", "Stop"]);
        assert_eq!(spec.matcher.as_deref(), Some("write_file"));
        assert_eq!(spec.command, "npm run lint");
    }

    /// 防回归：matcher 缺失字段使用 default（None）
    #[test]
    fn command_hook_spec_matcher_optional() {
        let raw = r#"{ "name":"x","phases":["Stop"],"command":"true" }"#;
        let spec: CommandHookSpec = serde_json::from_str(raw).unwrap();
        assert!(spec.matcher.is_none());
    }
}
