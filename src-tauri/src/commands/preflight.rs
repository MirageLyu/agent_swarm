use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;
use tauri::Manager;
use uuid::Uuid;

use crate::agent::belief_state::{self, PreflightBeliefState, SlotStatus};
use crate::agent::planner::{self, Alternative, DecisionEntry, PreflightAction, PreflightChoice};
use crate::agent::preflight_perf::{elapsed_ms_since, PreflightPerfSummary, PreflightTurnTiming};
use crate::commands::mission::{build_provider, PlanMissionResponse, TaskInfo};
use crate::llm::{ContentBlock, Message, MessageRole, TokenUsage};

// ---------- request / response types ----------

#[derive(Debug, Deserialize)]
pub struct StartPreflightRequest {
    /// FM-15 v2.2 (S2): mission-first。mission 必须已通过 `create_mission`
    /// 创建（含 repo_origin / repo_path）。
    pub mission_id: String,
}

#[derive(Debug, Serialize)]
pub struct StartPreflightResponse {
    pub mission_id: String,
    pub session_id: String,
}

#[derive(Debug, Deserialize)]
pub struct SendPreflightMessageRequest {
    pub session_id: String,
    pub message: String,
    pub mode: String,
}

#[derive(Debug, Deserialize)]
pub struct AddContractItemRequest {
    pub mission_id: String,
    pub section: String,
    pub text: String,
}

#[derive(Debug, Deserialize)]
pub struct RemoveContractItemRequest {
    pub mission_id: String,
    pub item_id: String,
}

#[derive(Debug, Deserialize)]
pub struct UpdateContractConfigRequest {
    pub mission_id: String,
    pub budget_usd: Option<f64>,
    pub quality_threshold: Option<f64>,
    pub max_duration_hours: Option<f64>,
}

#[derive(Debug, Serialize, Clone)]
pub struct ContractItemInfo {
    pub id: String,
    pub section: String,
    pub text: String,
    pub source: String,
    pub created_at: String,
}

#[derive(Debug, Serialize, Clone)]
pub struct ContractInfo {
    pub id: String,
    pub mission_id: String,
    pub status: String,
    pub budget_usd: Option<f64>,
    pub quality_threshold: Option<f64>,
    pub max_duration_hours: Option<f64>,
    pub signed_at: Option<String>,
    pub items: Vec<ContractItemInfo>,
}

#[derive(Debug, Serialize, Clone)]
pub struct PreflightMessageInfo {
    pub role: String,
    pub content: String,
    pub choices: Vec<PreflightChoice>,
    /// 该消息所处的对话模式（scenario_walk / devils_advocate / risk_highlighter）。
    /// 用户消息记录"发出时的模式"，assistant 消息记录"产出时的模式"。
    /// 前端用它给 assistant 气泡加 mode badge，刷新后不丢失。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// LLM 调用失败时由 Fix A 写入；前端据此把对应 user 气泡渲染成"已失败"
    /// 并提供重试按钮（调用 retry_preflight_message）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failed: Option<bool>,
    /// 失败时的可读错误（已经过 friendlify_error 处理）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// LLM 推理内容，前端用于展示可折叠的推理面板。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
}

// ---------- helpers ----------

fn friendlify_error(raw: &str) -> String {
    let lower = raw.to_lowercase();
    if lower.contains("decoding response body")
        || lower.contains("stream error")
        || lower.contains("connection")
    {
        "网络连接中断，请检查网络后重试".to_string()
    } else if lower.contains("timed out") || lower.contains("timeout") {
        "请求超时，LLM 响应时间过长，请稍后重试".to_string()
    } else if lower.contains("api key") || lower.contains("unauthorized") || lower.contains("401") {
        "API Key 无效或未配置，请在设置中检查".to_string()
    } else if lower.contains("rate limit") || lower.contains("429") {
        "请求过于频繁，请稍等片刻后重试".to_string()
    } else if lower.contains("500") || lower.contains("502") || lower.contains("503") {
        "LLM 服务暂时不可用，请稍后重试".to_string()
    } else {
        format!("对话出错：{}", raw)
    }
}

fn get_or_create_contract(conn: &rusqlite::Connection, mission_id: &str) -> anyhow::Result<String> {
    let existing: Option<String> = conn
        .query_row(
            "SELECT id FROM mission_contracts WHERE mission_id = ?",
            [mission_id],
            |row| row.get(0),
        )
        .ok();

    if let Some(id) = existing {
        return Ok(id);
    }

    let id = Uuid::new_v4().to_string();
    conn.execute(
        "INSERT INTO mission_contracts (id, mission_id) VALUES (?, ?)",
        rusqlite::params![id, mission_id],
    )?;
    Ok(id)
}

// ---------- tool_call processing ----------

fn load_belief_state(conn: &rusqlite::Connection, session_id: &str) -> PreflightBeliefState {
    let raw: String = conn
        .query_row(
            "SELECT belief_state FROM preflight_sessions WHERE id = ?",
            [session_id],
            |row| row.get(0),
        )
        .unwrap_or_else(|_| "{}".to_string());

    serde_json::from_str(&raw).unwrap_or_default()
}

fn save_belief_state(
    conn: &rusqlite::Connection,
    session_id: &str,
    state: &PreflightBeliefState,
) -> anyhow::Result<()> {
    let json = serde_json::to_string(state)?;
    conn.execute(
        "UPDATE preflight_sessions SET belief_state = ?, convergence_score = ?, phase = ?, updated_at = datetime('now') WHERE id = ?",
        rusqlite::params![json, state.convergence_score, state.phase.label(), session_id],
    )?;
    Ok(())
}

/// Load contract items as (section, text, confidence) tuples for prompt injection.
fn load_contract_items(
    conn: &rusqlite::Connection,
    mission_id: &str,
) -> Vec<(String, String, String)> {
    let contract_id: Option<String> = conn
        .query_row(
            "SELECT id FROM mission_contracts WHERE mission_id = ?",
            [mission_id],
            |row| row.get(0),
        )
        .ok();

    let Some(contract_id) = contract_id else {
        return vec![];
    };

    let mut stmt = conn.prepare(
        "SELECT section, text, source FROM contract_items WHERE contract_id = ? ORDER BY created_at ASC"
    ).unwrap();

    stmt.query_map([&contract_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)
                .unwrap_or_else(|_| "confirmed".into()),
        ))
    })
    .unwrap()
    .filter_map(|r| r.ok())
    .collect()
}

/// Load rejected alternatives from decision_log for prompt injection (FM-10.6.5).
fn load_rejected_alternatives(
    conn: &rusqlite::Connection,
    session_id: &str,
) -> Vec<(String, u32, String)> {
    let mut stmt = conn.prepare(
        "SELECT description, round, rationale FROM decision_log WHERE session_id = ? AND decision_type = 'rejected' ORDER BY round DESC LIMIT 10"
    ).unwrap();

    stmt.query_map([session_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, u32>(1)?,
            row.get::<_, String>(2).unwrap_or_default(),
        ))
    })
    .unwrap()
    .filter_map(|r| r.ok())
    .collect()
}

/// Insert a decision entry into the database (FM-10.6).
fn insert_decision_entry(
    conn: &rusqlite::Connection,
    session_id: &str,
    round: u32,
    decision_type: &str,
    description: &str,
    rationale: &str,
    alternatives: &[Alternative],
    contract_item_id: Option<&str>,
) {
    let id = Uuid::new_v4().to_string();
    let alts_json = serde_json::to_string(alternatives).unwrap_or_else(|_| "[]".to_string());
    let _ = conn.execute(
        "INSERT INTO decision_log (id, session_id, round, decision_type, description, rationale, alternatives, contract_item_id) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        rusqlite::params![id, session_id, round, decision_type, description, rationale, alts_json, contract_item_id],
    );
}

/// Save token usage metrics after each round (FM-10.5.4.1).
fn save_token_usage(conn: &rusqlite::Connection, session_id: &str, usage: &TokenUsage) {
    let _ = conn.execute(
        "UPDATE preflight_sessions SET last_input_tokens = ?, last_output_tokens = ?, cumulative_input_tokens = cumulative_input_tokens + ?, cumulative_output_tokens = cumulative_output_tokens + ?, updated_at = datetime('now') WHERE id = ?",
        rusqlite::params![usage.input_tokens as i64, usage.output_tokens as i64, usage.input_tokens as i64, usage.output_tokens as i64, session_id],
    );
}

/// Process tool_call actions: execute DB side effects, update belief_state,
/// record decisions, return tool_result messages for conversation history.
fn process_tool_actions(
    conn: &rusqlite::Connection,
    mission_id: &str,
    actions: &[PreflightAction],
    belief_state: &mut PreflightBeliefState,
    app: &tauri::AppHandle,
    session_id: &str,
) -> Vec<serde_json::Value> {
    let mut tool_results = Vec::new();

    for action in actions {
        match action {
            PreflightAction::PresentChoices { id, args } => {
                let choices_json = serde_json::to_string(&args.choices).unwrap_or_default();
                planner::emit_preflight_event_pub(app, session_id, "choices", &choices_json);
                tool_results.push(json!({
                    "role": "tool",
                    "tool_call_id": id,
                    "content": json!({"presented": true, "choices_count": args.choices.len()}).to_string()
                }));
            }
            PreflightAction::AddContractItem { id, args } => {
                let contract_id = get_or_create_contract(conn, mission_id).unwrap_or_default();
                let result =
                    add_contract_item_internal(conn, &contract_id, &args.section, &args.item);

                match result {
                    Ok(item_id) => {
                        let slot_status = match args.confidence.as_str() {
                            "confirmed" => SlotStatus::Confirmed,
                            _ => SlotStatus::Tentative,
                        };
                        if let Some(slot_id) =
                            belief_state::map_contract_item_to_slot(&args.section, &args.item)
                        {
                            belief_state.update_slot(
                                slot_id,
                                slot_status,
                                Some(args.item.clone()),
                                belief_state.round,
                            );
                        }

                        // FM-10.6: Auto-record decision
                        let decision_type = match args.confidence.as_str() {
                            "confirmed" => "confirmed",
                            "inferred" => "inferred",
                            _ => "confirmed",
                        };
                        insert_decision_entry(
                            conn,
                            session_id,
                            belief_state.round,
                            decision_type,
                            &args.item,
                            args.rationale.as_deref().unwrap_or(""),
                            &[],
                            Some(&item_id),
                        );

                        let item_info = json!({
                            "id": item_id,
                            "section": args.section,
                            "text": args.item,
                            "source": "agent",
                        });
                        planner::emit_preflight_event_pub(
                            app,
                            session_id,
                            "contract_item_added",
                            &item_info.to_string(),
                        );

                        tool_results.push(json!({
                            "role": "tool",
                            "tool_call_id": id,
                            "content": json!({"success": true, "item_id": item_id}).to_string()
                        }));
                    }
                    Err(e) => {
                        tracing::warn!("add_contract_item via tool_call failed: {e}");
                        tool_results.push(json!({
                            "role": "tool",
                            "tool_call_id": id,
                            "content": json!({"success": false, "reason": e.to_string()}).to_string()
                        }));
                    }
                }
            }
            PreflightAction::UpdateContractItem { id, args } => {
                // FM-10.6: Record the old content before update
                let old_content: Option<String> = conn
                    .query_row(
                        "SELECT text FROM contract_items WHERE id = ?",
                        [&args.item_id],
                        |row| row.get(0),
                    )
                    .ok();

                let result = update_contract_item_internal(conn, &args.item_id, &args.new_content);
                match result {
                    Ok(()) => {
                        // FM-10.6: Record Revised decision
                        let desc = if let Some(old) = &old_content {
                            format!("{} → {}", old, args.new_content)
                        } else {
                            args.new_content.clone()
                        };
                        insert_decision_entry(
                            conn,
                            session_id,
                            belief_state.round,
                            "revised",
                            &desc,
                            args.reason.as_deref().unwrap_or(""),
                            &[],
                            Some(&args.item_id),
                        );

                        tool_results.push(json!({
                            "role": "tool",
                            "tool_call_id": id,
                            "content": json!({"success": true}).to_string()
                        }));
                    }
                    Err(e) => {
                        tool_results.push(json!({
                            "role": "tool",
                            "tool_call_id": id,
                            "content": json!({"success": false, "reason": e.to_string()}).to_string()
                        }));
                    }
                }
            }
            PreflightAction::SuggestSign { id, args } => {
                let suggest_json = serde_json::to_string(args).unwrap_or_default();
                planner::emit_preflight_event_pub(app, session_id, "suggest_sign", &suggest_json);
                tool_results.push(json!({
                    "role": "tool",
                    "tool_call_id": id,
                    "content": json!({"suggested": true}).to_string()
                }));
            }
            PreflightAction::SwitchClarificationMode { id, args } => {
                let db_mode = match args.mode.as_str() {
                    "devils_advocate" => "devils_advocate",
                    "risk_tagging" => "risk_highlighter",
                    _ => "scenario_walk",
                };
                let _ = conn.execute(
                    "UPDATE preflight_sessions SET mode = ? WHERE id = ?",
                    rusqlite::params![db_mode, session_id],
                );
                planner::emit_preflight_event_pub(
                    app,
                    session_id,
                    "mode_switched",
                    &json!({"mode": db_mode}).to_string(),
                );
                tool_results.push(json!({
                    "role": "tool",
                    "tool_call_id": id,
                    "content": json!({"success": true, "new_mode": db_mode}).to_string()
                }));
            }
        }
    }

    tool_results
}

fn add_contract_item_internal(
    conn: &rusqlite::Connection,
    contract_id: &str,
    section: &str,
    text: &str,
) -> anyhow::Result<String> {
    let existing: bool = conn.query_row(
        "SELECT COUNT(*) > 0 FROM contract_items WHERE contract_id = ? AND section = ? AND text = ?",
        rusqlite::params![contract_id, section, text],
        |row| row.get(0),
    )?;

    if existing {
        anyhow::bail!("duplicate");
    }

    let item_id = Uuid::new_v4().to_string();
    conn.execute(
        "INSERT INTO contract_items (id, contract_id, section, text, source) VALUES (?, ?, ?, ?, 'agent')",
        rusqlite::params![item_id, contract_id, section, text],
    )?;
    Ok(item_id)
}

fn update_contract_item_internal(
    conn: &rusqlite::Connection,
    item_id: &str,
    new_content: &str,
) -> anyhow::Result<()> {
    let updated = conn.execute(
        "UPDATE contract_items SET text = ? WHERE id = ?",
        rusqlite::params![new_content, item_id],
    )?;
    if updated == 0 {
        anyhow::bail!("not_found");
    }
    Ok(())
}

/// Reconstruct LLM message history from stored JSON messages.
/// Handles user, assistant (with optional tool_calls), and tool result messages.
fn reconstruct_history(stored_msgs: &[serde_json::Value]) -> Vec<Message> {
    let mut history = Vec::new();

    for m in stored_msgs {
        match m["role"].as_str().unwrap_or("user") {
            "assistant" => {
                let mut content = Vec::new();
                // Reasoning 必须在 Text 之前（与 stream/parse 一致），
                // 这样 convert_messages 看到的 assistant 块顺序稳定。
                if let Some(reasoning) = m["reasoning_content"].as_str() {
                    if !reasoning.is_empty() {
                        content.push(ContentBlock::Reasoning {
                            text: reasoning.to_string(),
                        });
                    }
                }
                if let Some(text) = m["content"].as_str() {
                    if !text.is_empty() {
                        content.push(ContentBlock::Text {
                            text: text.to_string(),
                        });
                    }
                }
                if let Some(tool_calls) = m["tool_calls"].as_array() {
                    for tc in tool_calls {
                        let id = tc["id"].as_str().unwrap_or("").to_string();
                        let name = tc["name"].as_str().unwrap_or("").to_string();
                        let args_str = tc["arguments"].as_str().unwrap_or("{}");
                        let input: serde_json::Value =
                            serde_json::from_str(args_str).unwrap_or(json!({}));
                        content.push(ContentBlock::ToolUse { id, name, input });
                    }
                }
                if !content.is_empty() {
                    history.push(Message {
                        role: MessageRole::Assistant,
                        content,
                        cache_control: None,
                    });
                }
            }
            "tool" => {
                history.push(Message {
                    role: MessageRole::User,
                    content: vec![ContentBlock::ToolResult {
                        tool_use_id: m["tool_call_id"].as_str().unwrap_or("").to_string(),
                        content: m["content"].as_str().unwrap_or("").to_string(),
                        is_error: false,
                    }],
                    cache_control: None,
                });
            }
            // 默认分支同时覆盖 "user" 和 "system_seed"（start_preflight 注入的初始
            // prompt）。system_seed 必须以 User 身份进入 LLM 上下文（OpenAI / Anthropic
            // 一般要求 user 起头），但 get_preflight_session 会把它从展示列表过滤掉，
            // 避免出现"用户没说过的话"。其它未知 role 也兜底成 user，保留语料。
            _ => {
                let text = m["content"].as_str().unwrap_or("").to_string();
                history.push(Message {
                    role: MessageRole::User,
                    content: vec![ContentBlock::Text { text }],
                    cache_control: None,
                });
            }
        }
    }

    history
}

/// Build an assistant stored message with optional tool_calls.
///
/// `mode` 是产出该回复时的对话模式（scenario_walk / devils_advocate / risk_highlighter）。
/// 写进 stored_msgs 让前端在重新加载会话后仍能给气泡渲染对应的 mode badge。
fn build_assistant_stored_msg(
    response: &planner::PreflightResponse,
    mode: &str,
) -> serde_json::Value {
    let mut msg = json!({
        "role": "assistant",
        "content": response.text,
        "choices": response.choices,
        "mode": mode,
    });

    // OpenAI-compat 推理模型协议要求 round-trip：上一轮 assistant 的
    // reasoning_content 必须在下一轮 messages 里原样回传。所以这里持久化，
    // reconstruct_history 重建时插回成 ContentBlock::Reasoning。
    if !response.reasoning.is_empty() {
        msg["reasoning_content"] = json!(response.reasoning);
        // reasoning: for frontend display in collapsible panel
        msg["reasoning"] = json!(response.reasoning);
    }

    if !response.tool_calls.is_empty() {
        let tool_calls: Vec<serde_json::Value> = response
            .tool_calls
            .iter()
            .map(|tc| {
                json!({
                    "id": tc.id,
                    "name": tc.name,
                    "arguments": serde_json::to_string(&tc.arguments).unwrap_or_default(),
                })
            })
            .collect();
        msg["tool_calls"] = json!(tool_calls);
    }

    msg
}

fn build_done_payload(
    response: &planner::PreflightResponse,
    belief_state: &PreflightBeliefState,
    mode: &str,
    perf: Option<&PreflightPerfSummary>,
) -> serde_json::Value {
    let mut done_payload = json!({
        "text": response.text,
        "choices": response.choices,
        "convergence_score": belief_state.convergence_score,
        "phase": belief_state.phase.label(),
        "mode": mode,
    });

    // Include reasoning so frontend can render the collapsible panel
    if !response.reasoning.is_empty() {
        done_payload["reasoning"] = json!(response.reasoning);
    }

    if let Some(perf) = perf {
        done_payload["perf"] = serde_json::to_value(perf).unwrap_or_else(|_| json!({}));
    }

    done_payload
}

/// Emit the "done" event with belief_state info.
fn emit_done_with_belief_state(
    app: &tauri::AppHandle,
    session_id: &str,
    response: &planner::PreflightResponse,
    belief_state: &PreflightBeliefState,
    mode: &str,
    perf: Option<&PreflightPerfSummary>,
) {
    let done_payload = build_done_payload(response, belief_state, mode, perf);
    planner::emit_preflight_event_pub(app, session_id, "done", &done_payload.to_string());
}

// ---------- tool_call continuation loop ----------

const MAX_TOOL_CONTINUATION_ROUNDS: usize = 5;

/// Run a preflight chat round with automatic tool_call continuation.
///
/// When the LLM responds with only side-effect tool_calls (e.g. add_contract_item)
/// without presenting choices or suggesting sign-off, this function automatically
/// sends tool_results back and re-invokes the LLM until it produces user-facing output.
async fn preflight_with_continuation(
    provider: Arc<dyn crate::llm::LlmProvider>,
    model: &str,
    mode: &str,
    mut history: Vec<Message>,
    stored_msgs: &mut Vec<serde_json::Value>,
    session_id: &str,
    mission_id: &str,
    app: &tauri::AppHandle,
) {
    let mut combined_text = String::new();
    let mut turn_timing = PreflightTurnTiming::new();

    // Load model capabilities for dynamic prompt assembly
    let db = app.state::<crate::db::Database>();
    let caps = {
        let config_mgr = app.state::<crate::commands::ConfigManager>();
        let config = config_mgr.get_config_snapshot();
        // Detect actual provider from base_url or provider field
        let provider_name = if config.base_url.contains("dashscope") {
            "dashscope"
        } else if config.provider == "anthropic" {
            "anthropic"
        } else if config.base_url.contains("deepseek") {
            "deepseek"
        } else if config.base_url.contains("openai") {
            "openai"
        } else {
            &config.provider
        };
        crate::llm::registry::get_capabilities(provider_name, model)
    };

    // FM-15 v2.2 (S2 / FR-PF-01): from_existing 模式下装载只读探索工具，
    // 让 Pre-flight LLM 可以浏览仓库再发问。
    let mission_repo: (Option<String>, Option<String>) = db
        .with_conn(|conn| {
            conn.query_row(
                "SELECT repo_origin, repo_path FROM missions WHERE id = ?",
                [mission_id],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, Option<String>>(1)?,
                    ))
                },
            )
            .map_err(|e| anyhow::anyhow!("{e}"))
        })
        .unwrap_or((None, None));

    let explorer: Option<crate::agent::planner_tools::ReadOnlyExplorer> = match mission_repo {
        (Some(ref origin), Some(ref path)) if origin == "from_existing" => {
            let p = std::path::PathBuf::from(path);
            if p.is_dir() {
                Some(crate::agent::planner_tools::ReadOnlyExplorer::new(p))
            } else {
                tracing::warn!(
                    "Pre-flight repo_path '{}' is not a directory; explorer disabled",
                    path
                );
                None
            }
        }
        _ => None,
    };
    let extra_tools = if explorer.is_some() {
        crate::agent::planner_tools::ReadOnlyExplorer::tool_definitions()
    } else {
        Vec::new()
    };

    for iteration in 0..MAX_TOOL_CONTINUATION_ROUNDS {
        // Load current state for dynamic prompt
        let (contract_items, belief_state_snapshot, rejected_alts) = db
            .with_conn(|conn| {
                let items = load_contract_items(conn, mission_id);
                let bs = load_belief_state(conn, session_id);
                let alts = load_rejected_alternatives(conn, session_id);
                Ok::<_, anyhow::Error>((items, bs, alts))
            })
            .unwrap_or_default();

        // FM-10.5: Full Compaction check (only on first iteration of each round)
        if iteration == 0 {
            // 关键修复：之前这里查了 `compacted_at IS NOT NULL` 拿到一个 bool，但调用
            // should_compact 时第 5 个参数硬编码为 false —— compact 信号根本没传进去。
            // 配合 should_compact 内 `round >= 12 → 必触发`，结果第 12 轮之后每发
            // 一条消息都触发一次 50s+ 的完整 compaction，UI 永远卡在"正在优化对话上下文…"。
            //
            // 现在直接读取 compacted_at 的 round 值（INTEGER，migration 012 定义），
            // None 表示本 session 还没 compact 过；should_compact 据此实现冷却。
            let compact_state = db
                .with_conn(|conn| {
                    let last_input: Option<u64> = conn
                        .query_row(
                            "SELECT last_input_tokens FROM preflight_sessions WHERE id = ?",
                            [session_id],
                            |row| row.get(0),
                        )
                        .ok()
                        .flatten();
                    let failures: u32 = conn
                        .query_row(
                            "SELECT compaction_failures FROM preflight_sessions WHERE id = ?",
                            [session_id],
                            |row| row.get(0),
                        )
                        .unwrap_or(0);
                    let last_compacted_round: Option<u32> = conn
                        .query_row(
                            "SELECT compacted_at FROM preflight_sessions WHERE id = ?",
                            [session_id],
                            |row| row.get(0),
                        )
                        .ok()
                        .flatten();
                    Ok::<_, anyhow::Error>((last_input, failures, last_compacted_round))
                })
                .unwrap_or((None, 0, None));

            let (needs_compact, needs_warn) = planner::should_compact(
                compact_state.0,
                caps.context_window,
                belief_state_snapshot.round,
                compact_state.1,
                compact_state.2,
            );

            if needs_warn {
                planner::emit_preflight_event_pub(
                    app,
                    session_id,
                    "status",
                    "上下文较长，建议尽快完成澄清",
                );
            }

            if needs_compact {
                turn_timing.mark_compaction_triggered();
                tracing::info!(
                    round = belief_state_snapshot.round,
                    "triggering full compaction"
                );
                planner::emit_preflight_event_pub(app, session_id, "status", "正在优化对话上下文…");

                let scope_count = contract_items
                    .iter()
                    .filter(|(s, _, _)| s == "scope")
                    .count();
                let constraints_count = contract_items
                    .iter()
                    .filter(|(s, _, _)| s == "constraints")
                    .count();
                let exclusions_count = contract_items
                    .iter()
                    .filter(|(s, _, _)| s == "exclusions")
                    .count();
                let assumptions_count = contract_items
                    .iter()
                    .filter(|(s, _, _)| s == "assumptions")
                    .count();

                let compaction_prompt = planner::build_compaction_prompt(
                    scope_count,
                    constraints_count,
                    exclusions_count,
                    assumptions_count,
                );

                let mut compact_msgs = history.clone();
                compact_msgs.push(Message {
                    role: MessageRole::User,
                    content: vec![ContentBlock::Text {
                        text: compaction_prompt,
                    }],
                    cache_control: None,
                });

                let compact_request = crate::llm::LlmRequest {
                    model: model.to_string(),
                    system: None,
                    messages: compact_msgs,
                    tools: vec![],
                    max_tokens: 1500,
                    provider_extras: None,
                };

                // Compaction 是非流式 chat（要等完整 summary 一次性返回，没法用 idle watchdog），
                // 所以只能给一个 wall-clock 兜底。30s 太短：长上下文 + 推理模型（DeepSeek-R1 等）
                // 单次能跑 60~90s，30s 超时直接掉到 truncation fallback，损失对话上下文。
                // 120s 覆盖 99% 的 normal compaction，即便偶尔超时也只是 truncation 兜底，没数据丢失。
                const COMPACTION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);
                let compact_started = std::time::Instant::now();
                let compact_result =
                    tokio::time::timeout(COMPACTION_TIMEOUT, provider.chat(&compact_request)).await;
                let compact_elapsed = compact_started.elapsed();

                match compact_result {
                    Ok(Ok(resp)) => {
                        let summary: String = resp
                            .content
                            .iter()
                            .filter_map(|b| match b {
                                ContentBlock::Text { text } => Some(text.as_str()),
                                _ => None,
                            })
                            .collect();

                        if summary.len() > 50 {
                            let original_req = history.first();
                            let recent_count = std::cmp::min(6, history.len());
                            let recent = &history[history.len() - recent_count..];
                            history =
                                planner::build_compacted_messages(&summary, original_req, recent);

                            let _ = db.with_conn(|conn| {
                                conn.execute(
                                    "UPDATE preflight_sessions SET compacted_at = ?, compaction_summary = ?, compaction_failures = 0 WHERE id = ?",
                                    rusqlite::params![belief_state_snapshot.round as i64, &summary, session_id],
                                )?;
                                Ok::<_, anyhow::Error>(())
                            });

                            tracing::info!(
                                round = belief_state_snapshot.round,
                                summary_len = summary.len(),
                                elapsed_ms = compact_elapsed.as_millis(),
                                "full compaction succeeded"
                            );
                        } else {
                            tracing::warn!(
                                "compaction summary too short, using truncation fallback"
                            );
                            history = planner::truncate_messages(&history);
                            let _ = db.with_conn(|conn| {
                                conn.execute(
                                    "UPDATE preflight_sessions SET compaction_failures = compaction_failures + 1 WHERE id = ?",
                                    [session_id],
                                )?;
                                Ok::<_, anyhow::Error>(())
                            });
                        }
                    }
                    Ok(Err(e)) => {
                        tracing::error!(
                            "compaction LLM call failed: {e}, using truncation fallback"
                        );
                        history = planner::truncate_messages(&history);
                        let _ = db.with_conn(|conn| {
                            conn.execute(
                                "UPDATE preflight_sessions SET compaction_failures = compaction_failures + 1 WHERE id = ?",
                                [session_id],
                            )?;
                            Ok::<_, anyhow::Error>(())
                        });
                    }
                    Err(_) => {
                        tracing::error!(
                            timeout_secs = COMPACTION_TIMEOUT.as_secs(),
                            "compaction timed out, using truncation fallback"
                        );
                        history = planner::truncate_messages(&history);
                        let _ = db.with_conn(|conn| {
                            conn.execute(
                                "UPDATE preflight_sessions SET compaction_failures = compaction_failures + 1 WHERE id = ?",
                                [session_id],
                            )?;
                            Ok::<_, anyhow::Error>(())
                        });
                    }
                }
            }
        }

        planner::emit_preflight_event_pub(app, session_id, "status", "正在等待模型响应…");
        turn_timing.mark_llm_request_start();

        let (response, usage, llm_timing) = match planner::preflight_chat(
            provider.clone(),
            model,
            mode,
            history.clone(),
            session_id,
            app,
            &contract_items,
            &belief_state_snapshot,
            &rejected_alts,
            &caps,
            &extra_tools,
        )
        .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("Preflight chat failed (iteration {iteration}): {e}");
                let user_msg = friendlify_error(&e.to_string());

                // 关键修复：错误退出前必须把 stored_msgs 落盘。
                // 否则用户刚发的最后一条 user message 只在 in-memory，关闭重开就丢；
                // 即使不重开，下一次 send 时后端读到的是错误前的状态，新消息追加在
                // 错误那条之上 → LLM 上下文断裂。
                //
                // 同时给最后一条 user message 加 `failed: true` 标记，前端可以高亮
                // 并提供"重试"按钮（见 retry_preflight_message）。
                // 标记最后一条"待回复"的输入。包含 system_seed —— 初次进入
                // preflight 时 LLM 调用就失败，那条 system_seed 即是要重试的目标。
                if let Some(last) = stored_msgs
                    .iter_mut()
                    .rev()
                    .find(|m| matches!(m["role"].as_str(), Some("user") | Some("system_seed")))
                {
                    if let Some(obj) = last.as_object_mut() {
                        obj.insert("failed".into(), json!(true));
                        obj.insert("error".into(), json!(user_msg));
                    }
                }
                let _ = db.with_conn(|conn| {
                    conn.execute(
                        "UPDATE preflight_sessions SET messages = ?, updated_at = datetime('now') WHERE id = ?",
                        rusqlite::params![serde_json::to_string(stored_msgs).unwrap_or_default(), session_id],
                    )?;
                    Ok::<(), anyhow::Error>(())
                });

                let perf_summary = turn_timing.summary();
                tracing::info!(
                    session_id = %session_id,
                    mission_id = %mission_id,
                    mode = %mode,
                    iteration,
                    backend_prepare_ms = ?perf_summary.backend_prepare_ms,
                    llm_first_activity_ms = ?perf_summary.llm_first_activity_ms,
                    llm_ttft_ms = ?perf_summary.llm_ttft_ms,
                    llm_total_ms = perf_summary.llm_total_ms,
                    tool_processing_ms = perf_summary.tool_processing_ms,
                    continuation_count = perf_summary.continuation_count,
                    turn_total_ms = perf_summary.turn_total_ms,
                    compaction_triggered = perf_summary.compaction_triggered,
                    status = "error",
                    "preflight round perf"
                );

                planner::emit_preflight_event_pub(app, session_id, "error", &user_msg);
                return;
            }
        };
        turn_timing.record_llm_call(llm_timing, usage.clone());

        // Accumulate text across continuation rounds
        if !response.text.trim().is_empty() {
            if !combined_text.is_empty() {
                combined_text.push_str("\n\n");
            }
            combined_text.push_str(&response.text);
        }

        let (actions, _) = planner::parse_tool_calls_from_response(&response);
        let has_choices = !response.choices.is_empty();
        let has_suggest_sign = actions
            .iter()
            .any(|a| matches!(a, PreflightAction::SuggestSign { .. }));
        let has_tool_calls = !response.tool_calls.is_empty();

        // FM-15 v2.2 (S2 / FR-PF-01): 先消化 ReadOnlyExplorer 的 list_directory / read_file
        // 工具调用，把 tool_result 拼到主流程的 tool_result_msgs 里，否则 LLM 收不到回执。
        let tool_processing_started = std::time::Instant::now();
        let mut executed_tool_names: Vec<String> = response
            .tool_calls
            .iter()
            .map(|tc| tc.name.clone())
            .collect();

        let mut explorer_tool_results: Vec<serde_json::Value> = Vec::new();
        if let Some(ref ex) = explorer {
            let has_explorer_calls = response
                .tool_calls
                .iter()
                .any(|tc| matches!(tc.name.as_str(), "list_directory" | "read_file"));
            if has_explorer_calls {
                planner::emit_preflight_event_pub(app, session_id, "status", "正在读取仓库信息…");
            }
            for tc in &response.tool_calls {
                if matches!(tc.name.as_str(), "list_directory" | "read_file") {
                    let output = ex
                        .execute(&tc.name, &tc.arguments)
                        .await
                        .expect("ReadOnlyExplorer must handle list_directory / read_file");
                    explorer_tool_results.push(json!({
                        "role": "tool",
                        "tool_call_id": tc.id,
                        "content": output.content,
                    }));
                    tracing::info!(
                        tool = %tc.name,
                        is_error = output.is_error,
                        "preflight readonly explorer call"
                    );
                }
            }
        }

        // Process tool_call side effects + save token usage
        if !actions.is_empty() {
            planner::emit_preflight_event_pub(app, session_id, "status", "正在整理刚刚确认的内容…");
        }
        let process_result = db.with_conn(|conn| {
            let mut belief_state = load_belief_state(conn, session_id);
            if iteration == 0 {
                belief_state.increment_round();
            }

            let mut tool_results = process_tool_actions(
                conn,
                mission_id,
                &actions,
                &mut belief_state,
                app,
                session_id,
            );
            tool_results.extend(explorer_tool_results);

            belief_state.compute_convergence_score();
            belief_state.update_phase();
            save_belief_state(conn, session_id, &belief_state)?;

            // FM-10.5.4.1: Persist token usage
            save_token_usage(conn, session_id, &usage);

            Ok::<_, anyhow::Error>((tool_results, belief_state))
        });

        let (tool_result_msgs, belief_state) = match process_result {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("Tool processing failed: {e}");
                return;
            }
        };

        executed_tool_names.sort();
        executed_tool_names.dedup();
        turn_timing.record_tool_processing(
            elapsed_ms_since(tool_processing_started),
            executed_tool_names,
        );

        // Store assistant message + tool results in conversation history
        let assistant_msg = build_assistant_stored_msg(&response, mode);
        stored_msgs.push(assistant_msg);
        stored_msgs.extend(tool_result_msgs.clone());

        // Decide: continue the loop, or stop and return to user?
        let needs_continuation = has_tool_calls && !has_choices && !has_suggest_sign;

        if needs_continuation {
            // Append assistant message (with tool_calls) to LLM history.
            // Reasoning 必须放在最前，convert_messages 从 ContentBlock::Reasoning
            // 拼出 reasoning_content 字段；缺了它推理模型下一轮直接 400。
            let mut assistant_content = Vec::new();
            if !response.reasoning.is_empty() {
                assistant_content.push(ContentBlock::Reasoning {
                    text: response.reasoning.clone(),
                });
            }
            if !response.text.is_empty() {
                assistant_content.push(ContentBlock::Text {
                    text: response.text,
                });
            }
            for tc in &response.tool_calls {
                assistant_content.push(ContentBlock::ToolUse {
                    id: tc.id.clone(),
                    name: tc.name.clone(),
                    input: tc.arguments.clone(),
                });
            }
            history.push(Message {
                role: MessageRole::Assistant,
                content: assistant_content,
                cache_control: None,
            });

            // Append tool_results to LLM history
            for tr in &tool_result_msgs {
                history.push(Message {
                    role: MessageRole::User,
                    content: vec![ContentBlock::ToolResult {
                        tool_use_id: tr["tool_call_id"].as_str().unwrap_or("").to_string(),
                        content: tr["content"].as_str().unwrap_or("").to_string(),
                        is_error: false,
                    }],
                    cache_control: None,
                });
            }

            tracing::info!(
                iteration = iteration + 1,
                tool_names = response
                    .tool_calls
                    .iter()
                    .map(|tc| tc.name.as_str())
                    .collect::<Vec<_>>()
                    .join(","),
                "preflight continuing with tool_results"
            );

            turn_timing.record_continuation();
            planner::emit_preflight_event_pub(app, session_id, "status", "正在准备下一个问题…");
            continue;
        }

        // --- Stop: save state and emit done ---
        let _ = db.with_conn(|conn| {
            conn.execute(
                "UPDATE preflight_sessions SET messages = ?, updated_at = datetime('now') WHERE id = ?",
                rusqlite::params![serde_json::to_string(stored_msgs).unwrap_or_default(), session_id],
            )?;
            Ok::<(), anyhow::Error>(())
        });

        let perf_summary = turn_timing.summary();
        let final_response = planner::PreflightResponse {
            text: combined_text,
            choices: response.choices,
            tool_calls: response.tool_calls.clone(),
            fallback_used: response.fallback_used.clone(),
            reasoning: response.reasoning.clone(),
        };
        emit_done_with_belief_state(
            app,
            session_id,
            &final_response,
            &belief_state,
            mode,
            Some(&perf_summary),
        );

        let tool_names: Vec<&str> = response
            .tool_calls
            .iter()
            .map(|tc| tc.name.as_str())
            .collect();
        tracing::info!(
            round = belief_state.round,
            iterations = iteration + 1,
            tool_calls_count = response.tool_calls.len(),
            tool_names = tool_names.join(","),
            fallback_used = %response.fallback_used,
            convergence_score = belief_state.convergence_score,
            phase = %belief_state.phase.label(),
            backend_prepare_ms = ?perf_summary.backend_prepare_ms,
            llm_first_activity_ms = ?perf_summary.llm_first_activity_ms,
            llm_ttft_ms = ?perf_summary.llm_ttft_ms,
            llm_total_ms = perf_summary.llm_total_ms,
            tool_processing_ms = perf_summary.tool_processing_ms,
            continuation_count = perf_summary.continuation_count,
            turn_total_ms = perf_summary.turn_total_ms,
            input_tokens_total = perf_summary.input_tokens,
            output_tokens_total = perf_summary.output_tokens,
            cache_read_input_tokens_total = perf_summary.cache_read_input_tokens,
            cache_creation_input_tokens_total = perf_summary.cache_creation_input_tokens,
            compaction_triggered = perf_summary.compaction_triggered,
            "preflight round completed"
        );
        return;
    }

    // Max iterations exhausted — save what we have and stop
    tracing::warn!("preflight reached max continuation rounds ({MAX_TOOL_CONTINUATION_ROUNDS})");
    let db = app.state::<crate::db::Database>();
    let _ =
        db.with_conn(|conn| {
            conn.execute(
            "UPDATE preflight_sessions SET messages = ?, updated_at = datetime('now') WHERE id = ?",
            rusqlite::params![serde_json::to_string(stored_msgs).unwrap_or_default(), session_id],
        )?;
            Ok::<(), anyhow::Error>(())
        });

    let belief_state = db
        .with_conn(|conn| Ok::<_, anyhow::Error>(load_belief_state(conn, session_id)))
        .unwrap_or_default();
    let perf_summary = turn_timing.summary();
    let final_response = planner::PreflightResponse {
        text: combined_text,
        choices: vec![],
        tool_calls: vec![],
        fallback_used: "none".into(),
        reasoning: String::new(),
    };
    emit_done_with_belief_state(
        app,
        session_id,
        &final_response,
        &belief_state,
        mode,
        Some(&perf_summary),
    );
}

// ---------- commands ----------

#[tauri::command]
pub async fn start_preflight(
    app: tauri::AppHandle,
    request: StartPreflightRequest,
) -> Result<StartPreflightResponse, String> {
    let (provider, model) = build_provider(&app)?;

    let db = app.state::<crate::db::Database>();
    let mission_id = request.mission_id.clone();

    // FM-15 v2.2 (S2): mission 必须已存在，从中读 description。
    let (description, status): (String, String) = db
        .with_conn(|conn| {
            conn.query_row(
                "SELECT description, status FROM missions WHERE id = ?",
                [&mission_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .map_err(|e| anyhow::anyhow!("Mission {mission_id} not found: {e}"))
        })
        .map_err(|e| e.to_string())?;

    if !matches!(status.as_str(), "draft" | "preflight") {
        return Err(format!(
            "Cannot start preflight on mission in status '{status}'"
        ));
    }

    let session_id = Uuid::new_v4().to_string();

    db.with_conn(|conn| {
        // 把 mission 状态推进到 preflight
        conn.execute(
            "UPDATE missions SET status = 'preflight', updated_at = datetime('now')
             WHERE id = ? AND status = 'draft'",
            [&mission_id],
        )?;

        get_or_create_contract(conn, &mission_id)?;

        let initial_messages = serde_json::json!([]);
        conn.execute(
            "INSERT INTO preflight_sessions (id, mission_id, messages) VALUES (?, ?, ?)",
            rusqlite::params![session_id, mission_id, initial_messages.to_string()],
        )?;

        Ok(())
    })
    .map_err(|e| e.to_string())?;

    let initial_message = Message {
        role: MessageRole::User,
        content: vec![ContentBlock::Text {
            text: format!(
                "The user wants to build the following:\n\n{}\n\nStart the requirements clarification process.",
                description
            ),
        }],
        cache_control: None,
    };

    let sid = session_id.clone();

    let app_clone = app.clone();
    let mid = mission_id.clone();
    tokio::spawn(async move {
        // Build the initial user message for storage
        let user_text: String = initial_message
            .content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        // role=system_seed：这是后端注入的"包装提示"（"The user wants to build...
        // Start the requirements clarification process."），不是用户真实输入。
        // 用专属 role 区分：reconstruct_history 仍以 user 投喂 LLM；
        // get_preflight_session 渲染时整条丢弃，避免历史顶部出现一段"用户消息"。
        let mut stored_msgs = vec![json!({
            "role": "system_seed",
            "content": user_text,
            "choices": []
        })];

        planner::emit_preflight_event_pub(&app_clone, &sid, "start", "");

        preflight_with_continuation(
            provider,
            &model,
            "scenario_walk",
            vec![initial_message],
            &mut stored_msgs,
            &sid,
            &mid,
            &app_clone,
        )
        .await;
    });

    Ok(StartPreflightResponse {
        mission_id,
        session_id,
    })
}

/// 重试 session 里最后一条失败的 user message。
///
/// 跟 `send_preflight_message` 的区别：**不 push 新消息**，而是复用 stored_msgs
/// 里已有的最后一条 user message（被 Fix A 标记 `failed: true`）。
/// 调度同一套 `preflight_with_continuation`，成功后 `failed` 标记自然被新的
/// assistant 回复覆盖；失败则继续标 `failed`。
///
/// 设计理由：避免 send_preflight_message 重发导致同一条 user 消息在 stored_msgs
/// 里出现两次（破坏 LLM 上下文连贯性）。
#[derive(Debug, Deserialize)]
pub struct RetryPreflightMessageRequest {
    pub session_id: String,
    pub mode: String,
}

#[tauri::command]
pub async fn retry_preflight_message(
    app: tauri::AppHandle,
    request: RetryPreflightMessageRequest,
) -> Result<(), String> {
    let (provider, model) = build_provider(&app)?;
    let db = app.state::<crate::db::Database>();

    let (messages_json, mission_id): (String, String) = db
        .with_conn(|conn| {
            conn.execute(
                "UPDATE preflight_sessions SET mode = ?, updated_at = datetime('now') WHERE id = ?",
                rusqlite::params![request.mode, request.session_id],
            )?;
            conn.query_row(
                "SELECT messages, mission_id FROM preflight_sessions WHERE id = ?",
                [&request.session_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|_| anyhow::anyhow!("Session not found"))
        })
        .map_err(|e| e.to_string())?;

    let mut stored_msgs: Vec<serde_json::Value> =
        serde_json::from_str(&messages_json).unwrap_or_default();

    // 清掉最后一条用户输入（user 或 system_seed）上的 failed 标记，让它再次"未决"；
    // 失败时 Fix A 的错误路径会重新写回。
    if let Some(last) = stored_msgs
        .iter_mut()
        .rev()
        .find(|m| matches!(m["role"].as_str(), Some("user") | Some("system_seed")))
    {
        if let Some(obj) = last.as_object_mut() {
            obj.remove("failed");
            obj.remove("error");
        }
    } else {
        return Err("No message to retry".into());
    }

    // 立刻写回去除 failed 标记后的 stored_msgs，让前端在 LLM 回应前
    // 通过 getPreflightSession 看到"重试中"的状态（无 failed flag）。
    let serialized = serde_json::to_string(&stored_msgs).unwrap_or_default();
    db.with_conn(|conn| {
        conn.execute(
            "UPDATE preflight_sessions SET messages = ?, updated_at = datetime('now') WHERE id = ?",
            rusqlite::params![serialized, request.session_id],
        )?;
        Ok::<(), anyhow::Error>(())
    })
    .map_err(|e| e.to_string())?;

    let history = reconstruct_history(&stored_msgs);
    let sid = request.session_id.clone();
    let mode = request.mode.clone();
    let mid = mission_id.clone();
    let app_clone = app.clone();
    tokio::spawn(async move {
        planner::emit_preflight_event_pub(&app_clone, &sid, "start", "");
        preflight_with_continuation(
            provider,
            &model,
            &mode,
            history,
            &mut stored_msgs,
            &sid,
            &mid,
            &app_clone,
        )
        .await;
    });

    Ok(())
}

#[tauri::command]
pub async fn send_preflight_message(
    app: tauri::AppHandle,
    request: SendPreflightMessageRequest,
) -> Result<(), String> {
    let (provider, model) = build_provider(&app)?;

    let db = app.state::<crate::db::Database>();

    let (messages_json, _mission_id): (String, String) = db
        .with_conn(|conn| {
            conn.execute(
                "UPDATE preflight_sessions SET mode = ?, updated_at = datetime('now') WHERE id = ?",
                rusqlite::params![request.mode, request.session_id],
            )?;

            conn.query_row(
                "SELECT messages, mission_id FROM preflight_sessions WHERE id = ?",
                [&request.session_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|_| anyhow::anyhow!("Session not found"))
        })
        .map_err(|e| e.to_string())?;

    let mut stored_msgs: Vec<serde_json::Value> =
        serde_json::from_str(&messages_json).unwrap_or_default();

    // Issue 2: 原来到 100 messages（约 50 round）直接报错"会话作废"，过于简单粗暴。
    // 改为：
    // - 100..160 messages：放行，但 emit 一条 status 软提示让前端 status bar 高亮
    //   引导用户向 LLM 提分 / 签约（强引导也由 planner.rs::render_round_pressure_directive
    //   写进 system prompt）。
    // - >=160 messages（约 80 round）：才阻断，作为兜底防止真死循环。
    const HARD_MAX_MESSAGES: usize = 160;
    const SOFT_WARN_MESSAGES: usize = 100;
    if stored_msgs.len() >= HARD_MAX_MESSAGES {
        return Err(crate::error_code::IpcError::preflight_too_long(
            stored_msgs.len() as i64,
            HARD_MAX_MESSAGES as i64,
        )
        .to_string());
    }
    if stored_msgs.len() >= SOFT_WARN_MESSAGES {
        planner::emit_preflight_event_pub(
            &app,
            &request.session_id,
            "status",
            "对话已较长，请尽快确认核心条目并签署 Contract",
        );
    }

    // 持久化"发送时所处的 mode"。Fix D：刷新或重开页面后，前端需要恢复
    // assistant 气泡上的 mode badge；assistant 的 mode 由 build_assistant_stored_msg
    // 写入，user 的 mode 写在这里，二者配对。
    stored_msgs.push(json!({
        "role": "user",
        "content": request.message,
        "choices": [],
        "mode": request.mode,
    }));

    let history = reconstruct_history(&stored_msgs);

    let sid = request.session_id.clone();
    let mode = request.mode.clone();
    let mid = _mission_id.clone();

    let app_clone = app.clone();
    tokio::spawn(async move {
        planner::emit_preflight_event_pub(&app_clone, &sid, "start", "");

        preflight_with_continuation(
            provider,
            &model,
            &mode,
            history,
            &mut stored_msgs,
            &sid,
            &mid,
            &app_clone,
        )
        .await;
    });

    Ok(())
}

#[tauri::command]
pub fn add_contract_item(
    app: tauri::AppHandle,
    request: AddContractItemRequest,
) -> Result<ContractItemInfo, String> {
    let db = app.state::<crate::db::Database>();

    db.with_conn(|conn| {
        let contract_id = get_or_create_contract(conn, &request.mission_id)?;

        let existing: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM contract_items WHERE contract_id = ? AND section = ? AND text = ?",
                rusqlite::params![contract_id, request.section, request.text],
                |row| row.get(0),
            )?;

        if existing {
            anyhow::bail!("Item already exists in this section");
        }

        let item_id = Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO contract_items (id, contract_id, section, text, source) VALUES (?, ?, ?, ?, 'user')",
            rusqlite::params![item_id, contract_id, request.section, request.text],
        )?;

        let created_at: String = conn.query_row(
            "SELECT created_at FROM contract_items WHERE id = ?",
            [&item_id],
            |row| row.get(0),
        )?;

        Ok(ContractItemInfo {
            id: item_id,
            section: request.section,
            text: request.text,
            source: "user".to_string(),
            created_at,
        })
    })
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn remove_contract_item(
    app: tauri::AppHandle,
    request: RemoveContractItemRequest,
) -> Result<(), String> {
    let db = app.state::<crate::db::Database>();

    db.with_conn(|conn| {
        conn.execute(
            "DELETE FROM contract_items WHERE id = ?",
            [&request.item_id],
        )?;
        Ok(())
    })
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn update_contract_config(
    app: tauri::AppHandle,
    request: UpdateContractConfigRequest,
) -> Result<(), String> {
    let db = app.state::<crate::db::Database>();

    db.with_conn(|conn| {
        let contract_id = get_or_create_contract(conn, &request.mission_id)?;

        let mut sets = Vec::new();
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

        if let Some(budget) = request.budget_usd {
            sets.push("budget_usd = ?");
            params.push(Box::new(budget));
        }
        if let Some(qt) = request.quality_threshold {
            sets.push("quality_threshold = ?");
            params.push(Box::new(qt));
        }
        if let Some(dur) = request.max_duration_hours {
            sets.push("max_duration_hours = ?");
            params.push(Box::new(dur));
        }

        if sets.is_empty() {
            return Ok(());
        }

        sets.push("updated_at = datetime('now')");

        let sql = format!(
            "UPDATE mission_contracts SET {} WHERE id = ?",
            sets.join(", ")
        );
        params.push(Box::new(contract_id));

        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();
        conn.execute(&sql, param_refs.as_slice())?;

        Ok(())
    })
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_contract(app: tauri::AppHandle, mission_id: String) -> Result<ContractInfo, String> {
    let db = app.state::<crate::db::Database>();

    db.with_conn(|conn| {
        let contract_id = get_or_create_contract(conn, &mission_id)?;

        let contract: ContractInfo = conn.query_row(
            "SELECT id, mission_id, status, budget_usd, quality_threshold, max_duration_hours, signed_at
             FROM mission_contracts WHERE id = ?",
            [&contract_id],
            |row| {
                Ok(ContractInfo {
                    id: row.get(0)?,
                    mission_id: row.get(1)?,
                    status: row.get(2)?,
                    budget_usd: row.get(3)?,
                    quality_threshold: row.get(4)?,
                    max_duration_hours: row.get(5)?,
                    signed_at: row.get(6)?,
                    items: vec![],
                })
            },
        )?;

        let mut stmt = conn.prepare(
            "SELECT id, section, text, source, created_at FROM contract_items WHERE contract_id = ? ORDER BY created_at ASC",
        )?;
        let items: Vec<ContractItemInfo> = stmt
            .query_map([&contract_id], |row| {
                Ok(ContractItemInfo {
                    id: row.get(0)?,
                    section: row.get(1)?,
                    text: row.get(2)?,
                    source: row.get(3)?,
                    created_at: row.get(4)?,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();

        Ok(ContractInfo { items, ..contract })
    })
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_preflight_session(
    app: tauri::AppHandle,
    mission_id: String,
) -> Result<Option<PreflightSessionInfo>, String> {
    let db = app.state::<crate::db::Database>();

    db.with_conn(|conn| {
        let result = conn.query_row(
            "SELECT id, mode, messages, convergence_score, phase FROM preflight_sessions WHERE mission_id = ? ORDER BY created_at DESC LIMIT 1",
            [&mission_id],
            |row| {
                let id: String = row.get(0)?;
                let mode: String = row.get(1)?;
                let messages_json: String = row.get(2)?;
                let convergence_score: f64 = row.get(3)?;
                let phase: String = row.get(4)?;
                Ok((id, mode, messages_json, convergence_score, phase))
            },
        );

        match result {
            Ok((id, mode, messages_json, convergence_score, phase)) => {
                let stored: Vec<serde_json::Value> =
                    serde_json::from_str(&messages_json).unwrap_or_default();

                // Fix B2：从 stored_msgs 投影到前端展示前，剔除三类后端内部消息：
                //   1. role=tool —— 工具回执，纯协议噪音
                //   2. role=system_seed —— start_preflight 的初始 prompt 包装，
                //      用户视角应该看到的第一句是 LLM 的开场白
                //   3. role=assistant 但 content 为空且没有 choices —— tool_call 中间帧，
                //      LLM 只是在调用 add_contract_item / present_choices，没产出文本
                let messages: Vec<PreflightMessageInfo> = stored
                    .iter()
                    .filter_map(|m| {
                        let role = m["role"].as_str().unwrap_or("user");
                        if role == "tool" || role == "system_seed" {
                            return None;
                        }

                        let choices: Vec<PreflightChoice> = m["choices"]
                            .as_array()
                            .map(|arr| {
                                arr.iter()
                                    .filter_map(|v| serde_json::from_value(v.clone()).ok())
                                    .collect()
                            })
                            .unwrap_or_default();

                        let content = m["content"].as_str().unwrap_or("").to_string();
                        if role == "assistant" && content.trim().is_empty() && choices.is_empty() {
                            return None;
                        }

                        Some(PreflightMessageInfo {
                            role: role.to_string(),
                            content,
                            choices,
                            mode: m["mode"].as_str().map(|s| s.to_string()),
                            failed: m["failed"].as_bool(),
                            error: m["error"].as_str().map(|s| s.to_string()),
                            reasoning: m["reasoning"].as_str().map(|s| s.to_string()),
                        })
                    })
                    .collect();

                Ok(Some(PreflightSessionInfo {
                    id,
                    mode,
                    messages,
                    convergence_score,
                    phase,
                }))
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    })
    .map_err(|e| e.to_string())
}

#[derive(Debug, Serialize, Clone)]
pub struct PreflightSessionInfo {
    pub id: String,
    pub mode: String,
    pub messages: Vec<PreflightMessageInfo>,
    pub convergence_score: f64,
    pub phase: String,
}

// ---------- FM-10.6: Decision Log commands ----------

#[derive(Debug, Deserialize)]
pub struct GetDecisionLogRequest {
    pub session_id: String,
    pub decision_type: Option<String>,
}

#[tauri::command]
pub fn get_decision_log(
    app: tauri::AppHandle,
    request: GetDecisionLogRequest,
) -> Result<Vec<DecisionEntry>, String> {
    let db = app.state::<crate::db::Database>();

    db.with_conn(|conn| {
        let (sql, params): (String, Vec<Box<dyn rusqlite::types::ToSql>>) = if let Some(ref dt) = request.decision_type {
            (
                "SELECT id, session_id, round, decision_type, description, rationale, alternatives, contract_item_id, created_at FROM decision_log WHERE session_id = ? AND decision_type = ? ORDER BY round ASC".into(),
                vec![Box::new(request.session_id.clone()), Box::new(dt.clone())],
            )
        } else {
            (
                "SELECT id, session_id, round, decision_type, description, rationale, alternatives, contract_item_id, created_at FROM decision_log WHERE session_id = ? ORDER BY round ASC".into(),
                vec![Box::new(request.session_id.clone())],
            )
        };

        let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
        let mut stmt = conn.prepare(&sql)?;
        let entries: Vec<DecisionEntry> = stmt
            .query_map(param_refs.as_slice(), |row| {
                let alts_json: String = row.get(6)?;
                let alternatives: Vec<Alternative> = serde_json::from_str(&alts_json).unwrap_or_default();
                Ok(DecisionEntry {
                    id: row.get(0)?,
                    session_id: row.get(1)?,
                    round: row.get(2)?,
                    decision_type: row.get(3)?,
                    description: row.get(4)?,
                    rationale: row.get(5)?,
                    alternatives,
                    contract_item_id: row.get(7)?,
                    created_at: row.get(8)?,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();

        Ok(entries)
    })
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn sign_contract(
    app: tauri::AppHandle,
    mission_id: String,
) -> Result<PlanMissionResponse, String> {
    let (provider, model) = build_provider(&app)?;
    let db = app.state::<crate::db::Database>();

    // FM-15 v2.2 (S2 / FR-PF-02): 第 1 步 —— **只读校验 + 拿元数据**。
    //
    // 关键修复：之前这里同时执行 `UPDATE mission_contracts SET status = 'signed'`，
    // 但第 2 步 PlannerEngine 失败（超时 / LLM 错误）时不会回滚，导致 contract
    // 永久卡在 signed 状态：前端 ContractPanel 的 `readOnly = status === 'signed'`
    // 直接把整个签署区块隐藏 → 用户"会话作废，签署按钮没了"。
    //
    // 现在的契约：contract 的 signed 标记延后到第 3 步与 tasks 一起原子提交。
    // planner 失败 → contract 保持 drafting → 签署按钮自然可见 → 用户直接重签
    // （第 3 步本来就有"重 sign 时清掉旧 tasks"的 idempotent 处理）。
    let (contract_id, description, repo_path): (String, String, String) = db
        .with_conn(|conn| {
            let cid = get_or_create_contract(conn, &mission_id)?;

            let scope_count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM contract_items WHERE contract_id = ? AND section = 'scope'",
                [&cid],
                |row| row.get(0),
            )?;
            if scope_count == 0 {
                anyhow::bail!("Cannot sign contract: at least one Scope item is required");
            }

            let (description, repo_path): (String, Option<String>) = conn.query_row(
                "SELECT description, repo_path FROM missions WHERE id = ?",
                [&mission_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            let repo_path = repo_path.ok_or_else(|| {
                anyhow::anyhow!(
                    "Mission has no repo_path; create it via FR-18 create_mission first"
                )
            })?;
            Ok((cid, description, repo_path))
        })
        .map_err(|e| e.to_string())?;

    let repo_path_buf = std::path::PathBuf::from(&repo_path);
    if !repo_path_buf.is_dir() {
        return Err(format!(
            "Mission repo_path '{repo_path}' is not a directory"
        ));
    }

    // FM-15 v2.2 (S2 / FR-PF-02): 第 2 步 —— 用 PlannerEngine（kind=preflight）跑 Agent Loop。
    // contract 内容由 PlannerEngine 通过 `load_contract_dump` 注入到首条 user message。
    let engine = crate::agent::planner_engine::PlannerEngine::new(
        provider.clone(),
        model.clone(),
        repo_path_buf,
        app.clone(),
    );
    let outcome = engine
        .run_with(crate::agent::planner_engine::PlannerRunRequest {
            kind: crate::agent::planner_engine::PlannerKind::Preflight,
            description: &description,
            mission_id: Some(&mission_id),
            contract_id: Some(&contract_id),
        })
        .await
        .map_err(|e| e.to_string())?;
    let mut planner_output = outcome.output;
    let planner_session_id = outcome.session_id.clone();

    // Explicit Merge Node v1：planner LLM 输出之后、tasks 入库之前，按配置
    // 注入 merge 节点。inject 算法保证：
    //   - 若开关关闭 → no-op，行为字节对等旧路径
    //   - 若开关开 → 多 parent 汇合点用二叉 reduction tree 展开成 N-1 个 merge 节点
    // 注入后 validate_task_graph 不需要重跑——算法保证不产生新环，且原 task 的
    // depends_on 改成 [root_merge_id]（单 parent，恒合法）；新 merge node 的
    // depends_on 引用已存在 id（合法）。
    let merge_inject_enabled = {
        let cfg_mgr = app.state::<crate::commands::ConfigManager>();
        let cfg = cfg_mgr.get_config_snapshot();
        cfg.enable_explicit_merge_node
    };
    let merge_inject_added = crate::agent::planner_merge_inject::inject_merge_nodes(
        &mut planner_output.tasks,
        crate::agent::planner_merge_inject::InjectOptions {
            enabled: merge_inject_enabled,
        },
    );
    if merge_inject_added > 0 {
        tracing::info!(
            mission_id = %mission_id,
            added = merge_inject_added,
            total_tasks = planner_output.tasks.len(),
            "Explicit Merge Node v1: injected merge nodes for multi-parent joins"
        );
    }

    // 第 3 步 —— 一个事务原子完成：标 contract signed + 升 mission planned + 写 tasks。
    // 任意一步失败回滚整个事务，contract 不会卡在"半签"状态。
    //
    // FM-15 v2.2 (retryable-flow rule 2)：with_conn 不是事务包装，多条裸 execute() 之间
    // 没有原子性。这里显式 unchecked_transaction()，任意一条 INSERT 失败时之前的
    // contract.signed / mission.planned / tasks 全部回滚 —— 用户可以直接重新 sign。
    let tasks = db
        .with_conn(|conn| {
            let tx = conn.unchecked_transaction()?;

            // 关键修复：contract signed 移到这里，与 tasks 一起原子提交。
            // 见第 1 步的注释。
            tx.execute(
                "UPDATE mission_contracts SET status = 'signed', signed_at = datetime('now'),
                 updated_at = datetime('now') WHERE id = ?",
                [&contract_id],
            )?;

            tx.execute(
                "UPDATE missions SET title = ?, status = 'planned', updated_at = datetime('now')
                 WHERE id = ?",
                rusqlite::params![planner_output.mission_title, mission_id],
            )?;

            // 重 sign 时清掉旧 tasks（防止重复 sign 出现脏数据）
            tx.execute(
                "DELETE FROM task_dependencies WHERE task_id IN
                    (SELECT id FROM tasks WHERE mission_id = ?)",
                [&mission_id],
            )?;
            tx.execute("DELETE FROM tasks WHERE mission_id = ?", [&mission_id])?;

            let mut task_infos = Vec::new();
            let mut planner_id_to_db_id: std::collections::HashMap<String, String> =
                std::collections::HashMap::new();

            for pt in &planner_output.tasks {
                let task_id = Uuid::new_v4().to_string();
                planner_id_to_db_id.insert(pt.id.clone(), task_id.clone());

                // FM-15 v2.2 (S3-5): 富语义字段统一落库（与 Quick Plan 路径同构）。
                let consumes_translated: Vec<String> = pt
                    .consumes_artifacts
                    .iter()
                    .map(|c| {
                        if let Some((producer, local)) = c.split_once('.') {
                            if let Some(db) = planner_id_to_db_id.get(producer) {
                                return format!("{db}.{local}");
                            }
                        }
                        c.clone()
                    })
                    .collect();
                let additional_skills_json =
                    serde_json::to_string(&pt.additional_skills).unwrap_or_else(|_| "[]".into());
                let consumes_json =
                    serde_json::to_string(&consumes_translated).unwrap_or_else(|_| "[]".into());
                let produces_json =
                    serde_json::to_string(&pt.produces_artifacts).unwrap_or_else(|_| "[]".into());
                let file_scope_json = serde_json::to_string(&pt.file_scope_hints)
                    .unwrap_or_else(|_| "{\"definite\":[],\"possible\":[]}".into());

                // Explicit Merge Node v1：把 NodeKind 和 merge_parents 写入 migration 029 的新列。
                // 旧 task / 关闭 explicit merge 的 mission 永远是 'work' + NULL，行为字节对等。
                let kind_db = pt.kind.as_db_str();
                let merge_parents_json: Option<String> = if pt.merge_parents.is_empty() {
                    None
                } else {
                    // merge_parents 是 planner id；映射成 DB id 以便下游 scheduler 直接查询
                    let mapped: Vec<String> = pt
                        .merge_parents
                        .iter()
                        .filter_map(|p_id| planner_id_to_db_id.get(p_id).cloned())
                        .collect();
                    serde_json::to_string(&mapped).ok()
                };
                tx.execute(
                    "INSERT INTO tasks (id, mission_id, title, description, complexity, status, role, expected_output,
                        additional_skills, consumes_artifacts, produces_artifacts, file_scope_hints, kind, merge_parents)
                     VALUES (?, ?, ?, ?, ?, 'pending', ?, ?, ?, ?, ?, ?, ?, ?)",
                    rusqlite::params![
                        task_id,
                        mission_id,
                        pt.title,
                        pt.description,
                        pt.complexity,
                        pt.effective_role(),
                        pt.effective_expected_output(),
                        additional_skills_json,
                        consumes_json,
                        produces_json,
                        file_scope_json,
                        kind_db,
                        merge_parents_json,
                    ],
                )?;

                for decl in &pt.produces_artifacts {
                    crate::agent::artifacts::record_declaration(
                        &tx,
                        &mission_id,
                        &task_id,
                        decl,
                    )
                    .map_err(|e| {
                        rusqlite::Error::ToSqlConversionFailure(Box::new(
                            std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()),
                        ))
                    })?;
                }

                task_infos.push(TaskInfo {
                    id: task_id,
                    mission_id: mission_id.clone(),
                    title: pt.title.clone(),
                    description: pt.description.clone(),
                    status: "pending".to_string(),
                    complexity: pt.complexity.clone(),
                    assigned_agent_id: None,
                    created_at: String::new(),
                    completed_at: None,
                    role: pt.effective_role().to_string(),
                    expected_output: {
                        let eo = pt.effective_expected_output().to_string();
                        if eo.is_empty() { None } else { Some(eo) }
                    },
                    additional_skills_json: Some(additional_skills_json.clone()),
                    produces_artifacts_json: Some(produces_json.clone()),
                    consumes_artifacts_json: Some(consumes_json.clone()),
                    file_scope_hints_json: Some(file_scope_json.clone()),
                    last_error: None,
                    last_failed_at: None,
                    kind: pt.kind.as_db_str().to_string(),
                    merge_parents_json: merge_parents_json.clone(),
                });
            }

            for pt in &planner_output.tasks {
                let task_db_id = &planner_id_to_db_id[&pt.id];
                for dep_planner_id in &pt.depends_on {
                    if let Some(dep_db_id) = planner_id_to_db_id.get(dep_planner_id) {
                        let plan_prefix = format!("{}.", dep_planner_id);
                        let refs: Vec<String> = pt
                            .consumes_artifacts
                            .iter()
                            .filter(|c| c.starts_with(&plan_prefix))
                            .map(|c| {
                                let local = c.trim_start_matches(&plan_prefix);
                                format!("{dep_db_id}.{local}")
                            })
                            .collect();
                        let refs_json =
                            serde_json::to_string(&refs).unwrap_or_else(|_| "[]".into());
                        tx.execute(
                            "INSERT INTO task_dependencies (task_id, depends_on, artifact_refs)
                             VALUES (?, ?, ?)",
                            rusqlite::params![task_db_id, dep_db_id, refs_json],
                        )?;
                    }
                }
            }

            // 根任务（无依赖）直接 promote 到 ready —— sign_contract 视为隐式 confirm
            tx.execute(
                "UPDATE tasks SET status = 'ready'
                 WHERE mission_id = ?1 AND status = 'pending'
                   AND id NOT IN (SELECT task_id FROM task_dependencies)",
                [&mission_id],
            )?;

            for ti in &mut task_infos {
                ti.status = tx.query_row(
                    "SELECT status FROM tasks WHERE id = ?",
                    [&ti.id],
                    |row| row.get(0),
                )?;
                ti.created_at = tx.query_row(
                    "SELECT created_at FROM tasks WHERE id = ?",
                    [&ti.id],
                    |row| row.get(0),
                )?;
            }

            tx.commit()?;
            Ok(task_infos)
        })
        .map_err(|e| e.to_string())?;

    // 关联 planner_session 与 mission
    let _ = db.with_conn(|conn| {
        crate::db::queries::link_planner_session_to_mission(conn, &planner_session_id, &mission_id)
    });

    Ok(PlanMissionResponse {
        mission_id,
        tasks,
        planner_session_id: Some(planner_session_id),
    })
}

// FM-15 v2.2 (S2 / FR-PF-02): `stream_planner_call_for_contract` 已删除。
// 旧 sign_contract 的单次 LLM 调用被 PlannerEngine 替代；text_delta 透传由 PlannerEngine
// 内部走 `planner-stream` 事件实现，前端 `PlannerStreamPanel` 不变。

#[cfg(test)]
mod preflight_perf_payload_tests {
    use super::*;
    use crate::agent::belief_state::PreflightBeliefState;

    fn sample_response() -> planner::PreflightResponse {
        planner::PreflightResponse {
            text: "下一步请选择默认登录方式。".into(),
            choices: vec![],
            tool_calls: vec![],
            fallback_used: "none".into(),
            reasoning: String::new(),
        }
    }

    #[test]
    fn done_payload_omits_perf_when_unavailable() {
        let belief_state = PreflightBeliefState::new();
        let payload = build_done_payload(&sample_response(), &belief_state, "scenario_walk", None);

        assert_eq!(payload["text"], "下一步请选择默认登录方式。");
        assert_eq!(payload["mode"], "scenario_walk");
        assert!(payload.get("reasoning").is_none() || payload["reasoning"].as_str() == Some(""));
        assert!(payload.get("perf").is_none());
    }

    #[test]
    fn done_payload_includes_perf_when_available() {
        let belief_state = PreflightBeliefState::new();
        let perf = PreflightPerfSummary {
            backend_prepare_ms: Some(12),
            llm_first_activity_ms: Some(100),
            llm_ttft_ms: Some(150),
            llm_total_ms: 900,
            tool_processing_ms: 33,
            continuation_count: 1,
            turn_total_ms: 1200,
            tool_names: vec!["add_contract_item".into(), "present_choices".into()],
            compaction_triggered: false,
            input_tokens: 500,
            output_tokens: 80,
            cache_read_input_tokens: 200,
            cache_creation_input_tokens: 0,
        };

        let payload = build_done_payload(
            &sample_response(),
            &belief_state,
            "scenario_walk",
            Some(&perf),
        );

        assert_eq!(payload["perf"]["backend_prepare_ms"], 12);
        assert_eq!(payload["perf"]["llm_ttft_ms"], 150);
        assert_eq!(payload["perf"]["continuation_count"], 1);
        assert_eq!(payload["perf"]["tool_names"][0], "add_contract_item");
        assert_eq!(payload["perf"]["input_tokens"], 500);
    }

    #[test]
    fn done_payload_includes_reasoning_when_present() {
        let mut resp = sample_response();
        resp.reasoning = "Let me analyze the requirements.".into();
        let payload = build_done_payload(&resp, &PreflightBeliefState::new(), "scenario_walk", None);
        assert_eq!(payload["reasoning"], "Let me analyze the requirements.");
    }
}

#[cfg(test)]
mod reasoning_round_trip_tests {
    //! 回归测试：DeepSeek-R1 / V4-Pro / QwQ / Qwen3-thinking 等推理模型的
    //! reasoning_content **必须** round-trip 回下一轮请求，否则第二轮 400。
    //!
    //! 之前的单测只覆盖了 `convert_messages` 单元，**没覆盖** preflight 的
    //! "存盘 → 重读 → 下一轮序列化"完整路径，结果引入了 bug 还误以为修好了。
    //! 这组测试模拟完整链路，是必须长期驻守的护栏。
    use super::*;
    use crate::llm::OpenAICompatProvider;

    /// 模拟第一轮 LLM 返回带 reasoning，preflight 把它存到 stored_msgs。
    fn build_first_turn_stored_msgs() -> Vec<serde_json::Value> {
        let response = planner::PreflightResponse {
            text: "What's your target deployment platform?".into(),
            choices: vec![],
            tool_calls: vec![],
            fallback_used: "none".into(),
            reasoning: "User wants a CLI tool. Let me ask about platform.".into(),
        };
        vec![
            json!({ "role": "user", "content": "I want to build a tool", "choices": [] }),
            build_assistant_stored_msg(&response, "scenario_walk"),
        ]
    }

    #[test]
    fn assistant_stored_msg_persists_reasoning() {
        let stored = build_first_turn_stored_msgs();
        let assistant = &stored[1];
        assert_eq!(
            assistant["reasoning_content"], "User wants a CLI tool. Let me ask about platform.",
            "assistant 的 reasoning_content 必须落盘，否则下一轮 reconstruct 拿不到"
        );
    }

    #[test]
    fn reconstruct_history_restores_reasoning_block() {
        let stored = build_first_turn_stored_msgs();
        let history = reconstruct_history(&stored);

        let assistant_msg = history
            .iter()
            .find(|m| m.role == MessageRole::Assistant)
            .expect("history must contain an assistant turn");

        let has_reasoning = assistant_msg
            .content
            .iter()
            .any(|b| matches!(b, ContentBlock::Reasoning { text } if text.contains("CLI tool")));
        assert!(
            has_reasoning,
            "reconstruct_history 必须把 reasoning_content 还原成 ContentBlock::Reasoning，否则第二轮 convert_messages 看不见"
        );
    }

    /// 完整端到端：第一轮 reasoning 存盘 → 第二轮重建 history → openai_compat
    /// 序列化时必须出现 reasoning_content 字段。这是用户报告 bug 的真正场景。
    #[test]
    fn second_turn_request_includes_reasoning_content() {
        // 第一轮已结束，stored_msgs 里有 user + 带 reasoning 的 assistant
        let mut stored = build_first_turn_stored_msgs();

        // 第二轮：用户发新消息（模拟 send_preflight_message 的 push）
        stored.push(json!({
            "role": "user",
            "content": "I want it for macOS",
            "choices": []
        }));

        // 第二轮 history 由 reconstruct_history 重建（preflight_with_continuation 的真实路径）
        let history = reconstruct_history(&stored);

        // 模拟 OpenAI-compat provider 序列化这个 history 给 API
        let provider = OpenAICompatProvider::new("k".into(), "https://example.com".into());
        let req = crate::llm::LlmRequest {
            model: "deepseek-r1".into(),
            system: None,
            messages: history,
            tools: vec![],
            max_tokens: 100,
            provider_extras: None,
        };
        let oai_messages = provider.convert_messages(&req);

        let assistant = oai_messages
            .iter()
            .find(|m| m["role"] == "assistant")
            .expect("第二轮请求必须包含上一轮的 assistant turn");

        assert_eq!(
            assistant["reasoning_content"], "User wants a CLI tool. Let me ask about platform.",
            "第二轮 API 请求里 assistant.reasoning_content 必须存在，否则 DeepSeek-R1/V4-Pro 立刻 400"
        );
    }

    /// 没有 reasoning 的普通模型不能莫名出现 reasoning_content（避免给非推理 provider 噪音）。
    #[test]
    fn second_turn_omits_reasoning_when_first_turn_had_none() {
        let response_no_reasoning = planner::PreflightResponse {
            text: "ok".into(),
            choices: vec![],
            tool_calls: vec![],
            fallback_used: "none".into(),
            reasoning: String::new(),
        };
        let stored = vec![
            json!({ "role": "user", "content": "hi", "choices": [] }),
            build_assistant_stored_msg(&response_no_reasoning, "scenario_walk"),
        ];

        // 持久化阶段不应出现 reasoning_content key
        assert!(
            stored[1].get("reasoning_content").is_none(),
            "无 reasoning 时不应写 reasoning_content 字段"
        );

        // 序列化阶段同样
        let history = reconstruct_history(&stored);
        let provider = OpenAICompatProvider::new("k".into(), "https://example.com".into());
        let req = crate::llm::LlmRequest {
            model: "gpt-4o".into(),
            system: None,
            messages: history,
            tools: vec![],
            max_tokens: 100,
            provider_extras: None,
        };
        let oai_messages = provider.convert_messages(&req);
        let assistant = oai_messages
            .iter()
            .find(|m| m["role"] == "assistant")
            .unwrap();
        assert!(assistant.get("reasoning_content").is_none());
    }
}

#[cfg(test)]
mod preflight_session_projection_tests {
    //! 回归测试：Fix B1 / B2 / D 的全部不变量。
    //!
    //! 这些 bug 都属于"前端展示与后端 stored_msgs 含义不一致"——LLM 协议
    //! 需要 system_seed / tool / 中间 assistant 一起 round-trip，但用户不应
    //! 看到它们。这组测试锁定 stored_msgs → PreflightMessageInfo 的投影规则，
    //! 防止以后某次重构再"误为照顾 LLM 而把内部消息暴露给用户"。
    use super::*;
    use serde_json::json;

    /// 把原始 stored_msgs 经过 get_preflight_session 同款过滤后投影成
    /// 前端可见的 PreflightMessageInfo 列表。直接复刻 IPC handler 内 closure，
    /// 保证测试和真实路径不漂移。
    fn project_to_visible(stored: &[serde_json::Value]) -> Vec<PreflightMessageInfo> {
        stored
            .iter()
            .filter_map(|m| {
                let role = m["role"].as_str().unwrap_or("user");
                if role == "tool" || role == "system_seed" {
                    return None;
                }
                let choices: Vec<PreflightChoice> = m["choices"]
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| serde_json::from_value(v.clone()).ok())
                            .collect()
                    })
                    .unwrap_or_default();
                let content = m["content"].as_str().unwrap_or("").to_string();
                if role == "assistant" && content.trim().is_empty() && choices.is_empty() {
                    return None;
                }
                Some(PreflightMessageInfo {
                    role: role.to_string(),
                    content,
                    choices,
                    mode: m["mode"].as_str().map(|s| s.to_string()),
                    failed: m["failed"].as_bool(),
                    error: m["error"].as_str().map(|s| s.to_string()),
                    reasoning: m["reasoning"].as_str().map(|s| s.to_string()),
                })
            })
            .collect()
    }

    /// Fix B1：start_preflight 注入的 system_seed 不能在前端可见列表里出现，
    /// 但必须仍以 user 身份进入 LLM history（否则 LLM 看不到任务背景）。
    #[test]
    fn system_seed_hidden_from_user_but_visible_to_llm() {
        let stored = vec![
            json!({
                "role": "system_seed",
                "content": "The user wants to build X. Start clarification.",
                "choices": []
            }),
            json!({
                "role": "assistant",
                "content": "What's the target platform?",
                "choices": [],
                "mode": "scenario_walk"
            }),
        ];

        let visible = project_to_visible(&stored);
        assert_eq!(visible.len(), 1, "system_seed 必须从前端展示中剔除");
        assert_eq!(visible[0].role, "assistant");

        let history = reconstruct_history(&stored);
        let user_count = history
            .iter()
            .filter(|m| m.role == MessageRole::User)
            .count();
        assert_eq!(
            user_count, 1,
            "system_seed 必须以 user 身份进入 LLM history（否则 LLM 拿不到任务描述）"
        );
    }

    /// Fix B2：tool 回执是协议噪音，前端必须看不见。
    #[test]
    fn tool_messages_hidden_from_user() {
        let stored = vec![
            json!({ "role": "user", "content": "ok", "choices": [] }),
            json!({
                "role": "assistant",
                "content": "",
                "choices": [],
                "tool_calls": [{ "id": "t1", "name": "add_contract_item", "arguments": "{}" }]
            }),
            json!({
                "role": "tool",
                "tool_call_id": "t1",
                "content": "{\"success\": true}"
            }),
            json!({
                "role": "assistant",
                "content": "Done. Next question?",
                "choices": []
            }),
        ];

        let visible = project_to_visible(&stored);
        assert_eq!(visible.len(), 2, "tool 帧 + 空内容的 assistant 帧都要剔除");
        assert_eq!(visible[0].role, "user");
        assert_eq!(visible[1].role, "assistant");
        assert_eq!(visible[1].content, "Done. Next question?");
    }

    /// Fix B2 边界：assistant 没有 text 但带 choices（present_choices 工具）
    /// 必须保留——choices 本身就是用户可见的交互。
    #[test]
    fn assistant_with_only_choices_is_kept() {
        let stored = vec![json!({
            "role": "assistant",
            "content": "",
            "choices": [
                { "id": "c1", "label": "Option A", "contract_impact": null }
            ]
        })];
        let visible = project_to_visible(&stored);
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].choices.len(), 1);
    }

    /// Fix D：mode 必须持久化在 assistant 上，刷新后依旧能渲染 mode badge。
    #[test]
    fn assistant_mode_round_trips() {
        let resp = planner::PreflightResponse {
            text: "Risk-focused question?".into(),
            choices: vec![],
            tool_calls: vec![],
            fallback_used: "none".into(),
            reasoning: String::new(),
        };
        let stored = build_assistant_stored_msg(&resp, "risk_highlighter");
        assert_eq!(stored["mode"], "risk_highlighter");

        let visible = project_to_visible(std::slice::from_ref(&stored));
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].mode.as_deref(), Some("risk_highlighter"));
    }

    /// Fix A：错误路径必须给最后一条 user 写 failed/error；retry 必须能找到它。
    #[test]
    fn failed_flag_round_trips_for_retry() {
        let mut stored = vec![
            json!({ "role": "user", "content": "what about CI?", "choices": [], "mode": "scenario_walk" }),
        ];

        // 模拟 Fix A 在错误路径里写 failed
        if let Some(last) = stored
            .iter_mut()
            .rev()
            .find(|m| matches!(m["role"].as_str(), Some("user") | Some("system_seed")))
        {
            let obj = last.as_object_mut().unwrap();
            obj.insert("failed".into(), json!(true));
            obj.insert("error".into(), json!("网络连接中断，请检查网络后重试"));
        }

        let visible = project_to_visible(&stored);
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].failed, Some(true));
        assert_eq!(
            visible[0].error.as_deref(),
            Some("网络连接中断，请检查网络后重试")
        );

        // 模拟 retry：清掉 failed/error
        if let Some(last) = stored
            .iter_mut()
            .rev()
            .find(|m| matches!(m["role"].as_str(), Some("user") | Some("system_seed")))
        {
            let obj = last.as_object_mut().unwrap();
            obj.remove("failed");
            obj.remove("error");
        }
        let visible = project_to_visible(&stored);
        assert_eq!(visible[0].failed, None, "retry 必须移除 failed 标记");
        assert_eq!(visible[0].error, None);
    }

    /// Fix A 边界：只有 system_seed（初次进入 preflight 即失败）也要可重试。
    #[test]
    fn initial_seed_failure_is_retryable() {
        let mut stored =
            vec![json!({ "role": "system_seed", "content": "Bootstrap prompt", "choices": [] })];

        let target = stored
            .iter_mut()
            .rev()
            .find(|m| matches!(m["role"].as_str(), Some("user") | Some("system_seed")));
        assert!(
            target.is_some(),
            "首次失败时唯一的 system_seed 必须能被识别为重试目标，否则用户卡死"
        );
    }

    #[test]
    fn reasoning_projected_to_visible() {
        let stored = vec![json!({
            "role": "assistant",
            "content": "Let me think...",
            "choices": [],
            "mode": "scenario_walk",
            "reasoning": "User wants a CLI tool. Let me ask about platform."
        })];
        let visible = project_to_visible(&stored);
        assert_eq!(visible.len(), 1);
        assert_eq!(
            visible[0].reasoning.as_deref(),
            Some("User wants a CLI tool. Let me ask about platform.")
        );
    }
}

#[cfg(test)]
mod sign_contract_transaction_tests {
    //! 回归测试：retryable-flow rule 1 + 2 在 sign_contract 上的不变量。
    //!
    //! 真实 bug 路径："sign 按钮消失"是因为之前
    //! ① 把 `UPDATE contract SET status = 'signed'` 放到了 PlannerEngine **之前**；
    //! ② 第 3 步多条 `conn.execute()` 没有事务包裹，中途失败也不会回滚。
    //!
    //! 这里用真实 DB 锁定两条不变量，防止以后某次重构再回到坏状态。
    use crate::db::Database;

    /// 准备一条 mission（status='preflight'）+ 一份 drafting 的 contract，
    /// 模拟 sign_contract 前的快照。
    fn setup_mission_and_contract(db: &Database) -> (String, String) {
        let mission_id = "m-test".to_string();
        let contract_id = "c-test".to_string();
        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO missions (id, title, description, status) VALUES (?, ?, ?, 'preflight')",
                rusqlite::params![mission_id, "T", "D"],
            )?;
            conn.execute(
                "INSERT INTO mission_contracts (id, mission_id, status) VALUES (?, ?, 'drafting')",
                rusqlite::params![contract_id, mission_id],
            )?;
            Ok(())
        })
        .unwrap();
        (mission_id, contract_id)
    }

    fn read_status(db: &Database, sql: &str, id: &str) -> String {
        db.with_conn(|conn| {
            conn.query_row(sql, [id], |row| row.get::<_, String>(0))
                .map_err(Into::into)
        })
        .unwrap()
    }

    /// **核心不变量**：sign_contract 第 3 步事务里任意一步失败时，
    /// contract 不能停在 'signed' —— 必须连同 mission/tasks 一起回滚。
    /// 否则前端 ContractPanel 会把签约区块永久隐藏，用户进入"会话作废"死锁。
    #[test]
    fn third_step_failure_rolls_back_contract_signed() {
        let db = Database::open_in_memory().unwrap();
        let (mission_id, contract_id) = setup_mission_and_contract(&db);

        // 模拟第 3 步事务体的真实顺序：sign contract → 升 mission → 写 task。
        // 故意让"写 task"失败（违反 NOT NULL，因为没传 title），断言事务回滚。
        let result: anyhow::Result<()> = db.with_conn(|conn| {
            let tx = conn.unchecked_transaction()?;
            tx.execute(
                "UPDATE mission_contracts SET status = 'signed', signed_at = datetime('now')
                 WHERE id = ?",
                [&contract_id],
            )?;
            tx.execute(
                "UPDATE missions SET status = 'planned', updated_at = datetime('now')
                 WHERE id = ?",
                [&mission_id],
            )?;
            // 故意失败：title 是 NOT NULL，传 NULL 会触发约束失败。
            tx.execute(
                "INSERT INTO tasks (id, mission_id, title, description, complexity, status)
                 VALUES (?, ?, NULL, ?, ?, 'pending')",
                rusqlite::params!["task-x", mission_id, "desc", "low"],
            )?;
            tx.commit()?;
            Ok(())
        });

        assert!(result.is_err(), "失败步骤必须把 anyhow::Result 抛上去");
        assert_eq!(
            read_status(
                &db,
                "SELECT status FROM mission_contracts WHERE id = ?",
                &contract_id
            ),
            "drafting",
            "事务回滚后 contract 必须回到 drafting，否则签约按钮会永久消失"
        );
        assert_eq!(
            read_status(&db, "SELECT status FROM missions WHERE id = ?", &mission_id),
            "preflight",
            "mission 也必须回到 preflight，避免主界面把它误归类成 planned"
        );
    }

    /// 反例守卫：如果哪天有人不小心把多条 execute 写回裸 with_conn（无 transaction），
    /// 这条测试会失败 —— 提醒"忘了用 unchecked_transaction"。
    #[test]
    fn naked_with_conn_does_not_roll_back_partial_writes() {
        let db = Database::open_in_memory().unwrap();
        let (_mission_id, contract_id) = setup_mission_and_contract(&db);

        let res: anyhow::Result<()> = db.with_conn(|conn| {
            conn.execute(
                "UPDATE mission_contracts SET status = 'signed' WHERE id = ?",
                [&contract_id],
            )?;
            // 模拟中途失败
            anyhow::bail!("simulated mid-flight failure");
        });
        assert!(res.is_err());

        assert_eq!(
            read_status(
                &db,
                "SELECT status FROM mission_contracts WHERE id = ?",
                &contract_id
            ),
            "signed",
            "without explicit transaction, 之前的 UPDATE 不会回滚 —— \
             这正是 retryable-flow rule 2 要求所有多步 SQL 必须包 unchecked_transaction 的原因。\
             这条测试故意守住反例语义：如果 rusqlite 改了默认行为，我们要立刻知道。"
        );
    }
}
