//! FM-14: ApprovalGate —— 在 Coding Agent 调用工具 / 累计成本时插桩，
//! 把"潜在破坏性操作 / 预算超线"翻译为统一审批请求。
//!
//! 与 `agent::approval` 模块的分工：
//! - `approval` 是协调器 + DB 包装（通用）
//! - `approval_gate` 是 Coding Agent 专用的策略层（特定）
//!
//! 设计原则：
//! - **静默放行** > **错误**：策略关闭 / 不在 mission 上下文 / DB 临时不可用都视为放行，
//!   绝不让审批基础设施出问题就把整个 agent 拖死。
//! - **rejected 不抛 panic**：返回一个工具错误（is_error=true）让 LLM 自然进入"换种方式"逻辑。
//! - **budget 仅触发一次**：通过查 `approval_requests` 是否已有同 mission 的 budget 行避免轰炸。

use std::sync::Arc;

use serde_json::json;

use crate::agent::approval::{
    self, ApprovalCoordinator, ApprovalDecision, ApprovalKind, ApprovalRequestSpec,
};
use crate::commands::{ApprovalPolicy, ConfigManager};
use crate::db::Database;
use crate::tools::ToolOutput;

/// 工具拦截决策：调用方拿到后据此决定继续 / 替换为错误返回。
pub enum ToolGateOutcome {
    /// 放行（未命中策略 / 用户批准）
    Allow,
    /// 用户拒绝；调用方应当用此 ToolOutput 替代真正的 tool execution。
    Rejected(ToolOutput),
}

/// 当前 mission 是否启用了 budget 审批 + 当前累计是否触线。返回 Some((used, budget))
/// 表示触线，None 表示不触发。
fn budget_check(db: &Database, mission_id: &str, ratio: f32) -> Option<(f64, f64)> {
    if ratio <= 0.0 {
        return None;
    }
    db.with_conn(|conn| {
        let budget: Option<f64> = conn
            .query_row(
                "SELECT budget_usd FROM mission_contracts WHERE mission_id = ?1",
                [mission_id],
                |r| r.get::<_, Option<f64>>(0),
            )
            .ok()
            .flatten();
        let Some(budget) = budget else {
            return Ok::<Option<(f64, f64)>, anyhow::Error>(None);
        };
        let used: f64 = conn
            .query_row(
                "SELECT COALESCE(SUM(cost_usd), 0.0) FROM cost_records cr
                 JOIN agents a ON a.id = cr.agent_id
                 JOIN tasks t ON t.id = a.task_id
                 WHERE t.mission_id = ?1",
                [mission_id],
                |r| r.get::<_, f64>(0),
            )
            .unwrap_or(0.0);
        if budget > 0.0 && used >= budget * ratio as f64 {
            // 已经触发过同 mission 的 budget 审批（任意状态） → 不再重复
            let already: bool = conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM approval_requests
                                   WHERE mission_id = ?1 AND kind = 'budget')",
                    [mission_id],
                    |r| r.get::<_, bool>(0),
                )
                .unwrap_or(false);
            if already {
                return Ok(None);
            }
            return Ok(Some((used, budget)));
        }
        Ok(None)
    })
    .unwrap_or(None)
}

/// 拦截工具调用：命中策略则提交审批并阻塞等待。
///
/// 返回 `Allow` = 放行真正执行；`Rejected(out)` = 用 out 替代结果。
/// 出错（DB / coord 缺失等）一律视为 Allow，避免基础设施故障放大成业务故障。
pub async fn maybe_intercept_tool(
    app: &tauri::AppHandle,
    coord: &Arc<ApprovalCoordinator>,
    db: &Database,
    cancel: &tokio_util::sync::CancellationToken,
    mission_id: &str,
    agent_id: &str,
    tool_name: &str,
    input: &serde_json::Value,
) -> ToolGateOutcome {
    use tauri::Manager;
    let policy = match app.try_state::<ConfigManager>() {
        Some(c) => c.get_config_snapshot().approval_policy,
        None => return ToolGateOutcome::Allow,
    };

    let Some((title, payload, reason)) = classify_tool_call(&policy, tool_name, input) else {
        return ToolGateOutcome::Allow;
    };

    let timeout_secs = policy.timeout_seconds as i64;
    let mut spec = ApprovalRequestSpec::new(mission_id, ApprovalKind::Tool, title);
    spec.agent_id = Some(agent_id.to_string());
    spec.payload = payload;
    spec.reason = reason;
    spec.context_summary = format!("Tool: {tool_name}");
    spec.timeout_seconds = Some(timeout_secs);

    let outcome = match approval::submit_and_wait(coord, db, &spec, cancel).await {
        Ok((_id, o)) => o,
        Err(e) => {
            tracing::warn!("[approval_gate] submit_and_wait failed: {e}; allowing tool");
            return ToolGateOutcome::Allow;
        }
    };

    match outcome.decision {
        ApprovalDecision::Approved => ToolGateOutcome::Allow,
        ApprovalDecision::Rejected => ToolGateOutcome::Rejected(reject_tool_output(
            tool_name,
            outcome.note.as_deref(),
        )),
        ApprovalDecision::Expired => ToolGateOutcome::Rejected(ToolOutput {
            content: json!({
                "error": "approval_expired",
                "message": format!(
                    "User did not approve `{tool_name}` within the time limit. \
                     Try a less invasive approach or ask for confirmation explicitly."
                )
            })
            .to_string(),
            is_error: true,
        }),
        ApprovalDecision::Cancelled => ToolGateOutcome::Rejected(ToolOutput {
            content: json!({
                "error": "approval_cancelled",
                "message": "Mission was stopped before the approval was decided."
            })
            .to_string(),
            is_error: true,
        }),
    }
}

/// 命中策略时返回 (title, payload_json, reason)；否则 None。
pub fn classify_tool_call(
    policy: &ApprovalPolicy,
    tool_name: &str,
    input: &serde_json::Value,
) -> Option<(String, String, String)> {
    match tool_name {
        "write_file" => {
            let path = input.get("path").and_then(|v| v.as_str()).unwrap_or("");
            if path.is_empty() {
                return None;
            }
            if !path_matches(&policy.protected_paths, path) {
                return None;
            }
            Some((
                format!("Write to protected path: {path}"),
                json!({
                    "tool_name": "write_file",
                    "summary": format!("path = {path}"),
                    "input_preview": input.to_string().chars().take(280).collect::<String>(),
                })
                .to_string(),
                format!("`{path}` is on the protected paths list."),
            ))
        }
        "shell_exec" => {
            let cmd = input.get("command").and_then(|v| v.as_str()).unwrap_or("");
            if cmd.is_empty() {
                return None;
            }
            let matched = match_destructive(&policy.destructive_commands, cmd);
            matched.map(|hit| {
                (
                    format!("Run destructive command: {hit}"),
                    json!({
                        "tool_name": "shell_exec",
                        "summary": format!("$ {}", truncate(cmd, 200)),
                        "input_preview": cmd,
                    })
                    .to_string(),
                    format!("Command starts with `{hit}`, which is in the destructive list."),
                )
            })
        }
        _ => None,
    }
}

fn reject_tool_output(tool_name: &str, note: Option<&str>) -> ToolOutput {
    let extra = match note {
        Some(n) if !n.trim().is_empty() => format!(" User said: \"{}\"", n.trim()),
        _ => String::new(),
    };
    ToolOutput {
        content: json!({
            "error": "approval_rejected",
            "message": format!(
                "User rejected `{tool_name}`.{extra} Adjust your approach \
                 (e.g. use a different file or non-destructive command) and try again."
            )
        })
        .to_string(),
        is_error: true,
    }
}

fn path_matches(prefixes: &[String], path: &str) -> bool {
    let p = path.trim_start_matches("./");
    prefixes.iter().any(|prefix| {
        let prefix = prefix.trim_start_matches("./").trim_start_matches('/');
        if prefix.ends_with('/') {
            p.starts_with(prefix)
        } else {
            p == prefix || p.starts_with(&format!("{prefix}/"))
        }
    })
}

fn match_destructive(list: &[String], command: &str) -> Option<String> {
    let cmd_lower = command.trim().to_lowercase();
    for entry in list {
        let entry_norm = entry.trim().to_lowercase();
        if entry_norm.is_empty() {
            continue;
        }
        let needs_boundary = !entry_norm.contains(' ');
        if needs_boundary {
            // "rm" 命中 "rm" / "rm -rf x"，但不命中 "rmdir"
            let mut parts = cmd_lower.split_ascii_whitespace();
            if parts.next() == Some(entry_norm.as_str()) {
                return Some(entry_norm);
            }
        } else if cmd_lower.starts_with(&entry_norm) {
            // "git push" 整段匹配
            return Some(entry_norm);
        }
    }
    None
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect::<String>() + "…"
}

/// 触发 budget 审批（如果 ratio>0 且累计达线 且未触发过）。
/// 同步阻塞当前 step 直到决议（rejected → 调用方应当 stop mission）。
pub async fn maybe_trigger_budget(
    app: &tauri::AppHandle,
    coord: &Arc<ApprovalCoordinator>,
    db: &Database,
    cancel: &tokio_util::sync::CancellationToken,
    mission_id: &str,
    agent_id: &str,
) -> Option<ApprovalDecision> {
    use tauri::Manager;
    let policy = app.try_state::<ConfigManager>()?.get_config_snapshot().approval_policy;
    let (used, budget) = budget_check(db, mission_id, policy.budget_warn_ratio)?;
    let timeout_secs = policy.timeout_seconds as i64;
    let pct = ((used / budget) * 100.0).round();
    let title = format!("Budget {pct:.0}% used (${used:.2} of ${budget:.2})");
    let mut spec = ApprovalRequestSpec::new(mission_id, ApprovalKind::Budget, title);
    spec.agent_id = Some(agent_id.to_string());
    spec.payload = json!({
        "used_usd": used,
        "budget_usd": budget,
        "ratio": policy.budget_warn_ratio,
    })
    .to_string();
    spec.reason = format!(
        "Mission has consumed ≥ {pct:.0}% of the contracted budget. Approve to keep going, \
         reject to stop the mission."
    );
    spec.timeout_seconds = Some(timeout_secs);

    match approval::submit_and_wait(coord, db, &spec, cancel).await {
        Ok((_id, o)) => Some(o.decision),
        Err(e) => {
            tracing::warn!("[approval_gate] budget submit failed: {e}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pol() -> ApprovalPolicy {
        ApprovalPolicy {
            timeout_seconds: 600,
            protected_paths: vec!["package.json".into(), ".github/".into(), "src/critical.rs".into()],
            destructive_commands: vec!["rm".into(), "git push".into(), "npm publish".into()],
            budget_warn_ratio: 0.8,
            chat_commit_soft_lines: 10,
        }
    }

    #[test]
    fn classify_write_file_protected() {
        let r = classify_tool_call(
            &pol(),
            "write_file",
            &json!({"path": "package.json", "content": "{}"}),
        );
        assert!(r.is_some());
        let (title, _, reason) = r.unwrap();
        assert!(title.contains("package.json"));
        assert!(reason.contains("protected"));
    }

    #[test]
    fn classify_write_file_under_protected_dir() {
        let r = classify_tool_call(
            &pol(),
            "write_file",
            &json!({"path": ".github/workflows/ci.yml", "content": "..."}),
        );
        assert!(r.is_some());
    }

    #[test]
    fn classify_write_file_not_protected() {
        let r = classify_tool_call(
            &pol(),
            "write_file",
            &json!({"path": "src/utils.rs", "content": "..."}),
        );
        assert!(r.is_none());
    }

    #[test]
    fn classify_shell_destructive_word_boundary() {
        let r = classify_tool_call(&pol(), "shell_exec", &json!({"command": "rm -rf dist"}));
        assert!(r.is_some());
    }

    #[test]
    fn classify_shell_no_partial_match() {
        // "rmdir" 不应被 "rm" 误命中
        let r = classify_tool_call(&pol(), "shell_exec", &json!({"command": "rmdir build"}));
        assert!(r.is_none());
    }

    #[test]
    fn classify_shell_multiword_prefix() {
        let r = classify_tool_call(
            &pol(),
            "shell_exec",
            &json!({"command": "git push origin main"}),
        );
        assert!(r.is_some());
        let (title, _, _) = r.unwrap();
        assert!(title.contains("git push"));
    }

    #[test]
    fn classify_safe_shell_passes() {
        let r = classify_tool_call(&pol(), "shell_exec", &json!({"command": "cargo test"}));
        assert!(r.is_none());
    }

    #[test]
    fn classify_unknown_tool_passes() {
        let r = classify_tool_call(&pol(), "read_file", &json!({"path": "package.json"}));
        assert!(r.is_none());
    }
}
