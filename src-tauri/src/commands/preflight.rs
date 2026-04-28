use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;
use tauri::Manager;
use uuid::Uuid;

use crate::agent::belief_state::{self, PreflightBeliefState, SlotStatus};
use crate::agent::planner::{self, PreflightAction, PreflightChoice, DecisionEntry, Alternative};
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
}

// ---------- helpers ----------

fn friendlify_error(raw: &str) -> String {
    let lower = raw.to_lowercase();
    if lower.contains("decoding response body") || lower.contains("stream error") || lower.contains("connection") {
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

fn get_or_create_contract(
    conn: &rusqlite::Connection,
    mission_id: &str,
) -> anyhow::Result<String> {
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

fn load_belief_state(
    conn: &rusqlite::Connection,
    session_id: &str,
) -> PreflightBeliefState {
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

    let Some(contract_id) = contract_id else { return vec![] };

    let mut stmt = conn.prepare(
        "SELECT section, text, source FROM contract_items WHERE contract_id = ? ORDER BY created_at ASC"
    ).unwrap();

    stmt.query_map([&contract_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2).unwrap_or_else(|_| "confirmed".into()),
        ))
    }).unwrap().filter_map(|r| r.ok()).collect()
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
    }).unwrap().filter_map(|r| r.ok()).collect()
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
fn save_token_usage(
    conn: &rusqlite::Connection,
    session_id: &str,
    usage: &TokenUsage,
) {
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
                let result = add_contract_item_internal(conn, &contract_id, &args.section, &args.item);

                match result {
                    Ok(item_id) => {
                        let slot_status = match args.confidence.as_str() {
                            "confirmed" => SlotStatus::Confirmed,
                            _ => SlotStatus::Tentative,
                        };
                        if let Some(slot_id) = belief_state::map_contract_item_to_slot(&args.section, &args.item) {
                            belief_state.update_slot(slot_id, slot_status, Some(args.item.clone()), belief_state.round);
                        }

                        // FM-10.6: Auto-record decision
                        let decision_type = match args.confidence.as_str() {
                            "confirmed" => "confirmed",
                            "inferred" => "inferred",
                            _ => "confirmed",
                        };
                        insert_decision_entry(
                            conn, session_id, belief_state.round, decision_type,
                            &args.item,
                            args.rationale.as_deref().unwrap_or(""),
                            &[], Some(&item_id),
                        );

                        let item_info = json!({
                            "id": item_id,
                            "section": args.section,
                            "text": args.item,
                            "source": "agent",
                        });
                        planner::emit_preflight_event_pub(
                            app, session_id, "contract_item_added",
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
                    .query_row("SELECT text FROM contract_items WHERE id = ?", [&args.item_id], |row| row.get(0))
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
                            conn, session_id, belief_state.round, "revised",
                            &desc,
                            args.reason.as_deref().unwrap_or(""),
                            &[], Some(&args.item_id),
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
                    app, session_id, "mode_switched",
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
                        tool_use_id: m["tool_call_id"]
                            .as_str()
                            .unwrap_or("")
                            .to_string(),
                        content: m["content"].as_str().unwrap_or("").to_string(),
                        is_error: false,
                    }],
                    cache_control: None,
                });
            }
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
fn build_assistant_stored_msg(
    response: &planner::PreflightResponse,
) -> serde_json::Value {
    let mut msg = json!({
        "role": "assistant",
        "content": response.text,
        "choices": response.choices,
    });

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

/// Emit the "done" event with belief_state info.
fn emit_done_with_belief_state(
    app: &tauri::AppHandle,
    session_id: &str,
    response: &planner::PreflightResponse,
    belief_state: &PreflightBeliefState,
    mode: &str,
) {
    let done_payload = json!({
        "text": response.text,
        "choices": response.choices,
        "convergence_score": belief_state.convergence_score,
        "phase": belief_state.phase.label(),
        "mode": mode,
    });
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
        let (contract_items, belief_state_snapshot, rejected_alts) = db.with_conn(|conn| {
            let items = load_contract_items(conn, mission_id);
            let bs = load_belief_state(conn, session_id);
            let alts = load_rejected_alternatives(conn, session_id);
            Ok::<_, anyhow::Error>((items, bs, alts))
        }).unwrap_or_default();

        // FM-10.5: Full Compaction check (only on first iteration of each round)
        if iteration == 0 {
            let compact_state = db.with_conn(|conn| {
                let last_input: Option<u64> = conn
                    .query_row("SELECT last_input_tokens FROM preflight_sessions WHERE id = ?", [session_id], |row| row.get(0))
                    .ok().flatten();
                let failures: u32 = conn
                    .query_row("SELECT compaction_failures FROM preflight_sessions WHERE id = ?", [session_id], |row| row.get(0))
                    .unwrap_or(0);
                let already_compacted: bool = conn
                    .query_row("SELECT compacted_at IS NOT NULL FROM preflight_sessions WHERE id = ?", [session_id], |row| row.get(0))
                    .unwrap_or(false);
                Ok::<_, anyhow::Error>((last_input, failures, already_compacted))
            }).unwrap_or((None, 0, false));

            let (needs_compact, needs_warn) = planner::should_compact(
                compact_state.0,
                caps.context_window,
                belief_state_snapshot.round,
                compact_state.1,
                false,
            );

            if needs_warn {
                planner::emit_preflight_event_pub(app, session_id, "status", "上下文较长，建议尽快完成澄清");
            }

            if needs_compact {
                tracing::info!(round = belief_state_snapshot.round, "triggering full compaction");
                planner::emit_preflight_event_pub(app, session_id, "status", "正在优化对话上下文…");

                let scope_count = contract_items.iter().filter(|(s, _, _)| s == "scope").count();
                let constraints_count = contract_items.iter().filter(|(s, _, _)| s == "constraints").count();
                let exclusions_count = contract_items.iter().filter(|(s, _, _)| s == "exclusions").count();
                let assumptions_count = contract_items.iter().filter(|(s, _, _)| s == "assumptions").count();

                let compaction_prompt = planner::build_compaction_prompt(
                    scope_count, constraints_count, exclusions_count, assumptions_count,
                );

                let mut compact_msgs = history.clone();
                compact_msgs.push(Message {
                    role: MessageRole::User,
                    content: vec![ContentBlock::Text { text: compaction_prompt }],
                    cache_control: None,
                });

                let compact_request = crate::llm::LlmRequest {
                    model: model.to_string(),
                    system: None,
                    messages: compact_msgs,
                    tools: vec![],
                    max_tokens: 1500,
                };

                let compact_result = tokio::time::timeout(
                    std::time::Duration::from_secs(30),
                    provider.chat(&compact_request),
                ).await;

                match compact_result {
                    Ok(Ok(resp)) => {
                        let summary: String = resp.content.iter().filter_map(|b| match b {
                            ContentBlock::Text { text } => Some(text.as_str()),
                            _ => None,
                        }).collect();

                        if summary.len() > 50 {
                            let original_req = history.first();
                            let recent_count = std::cmp::min(6, history.len());
                            let recent = &history[history.len() - recent_count..];
                            history = planner::build_compacted_messages(&summary, original_req, recent);

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
                                "full compaction succeeded"
                            );
                        } else {
                            tracing::warn!("compaction summary too short, using truncation fallback");
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
                        tracing::error!("compaction LLM call failed: {e}, using truncation fallback");
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
                        tracing::error!("compaction timed out (>30s), using truncation fallback");
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

        let (response, usage) = match planner::preflight_chat(
            provider.clone(), model, mode, history.clone(), session_id, app,
            &contract_items, &belief_state_snapshot, &rejected_alts, &caps,
            &extra_tools,
        ).await {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("Preflight chat failed (iteration {iteration}): {e}");
                let user_msg = friendlify_error(&e.to_string());
                planner::emit_preflight_event_pub(app, session_id, "error", &user_msg);
                return;
            }
        };

        // Accumulate text across continuation rounds
        if !response.text.trim().is_empty() {
            if !combined_text.is_empty() {
                combined_text.push_str("\n\n");
            }
            combined_text.push_str(&response.text);
        }

        let (actions, _) = planner::parse_tool_calls_from_response(&response);
        let has_choices = !response.choices.is_empty();
        let has_suggest_sign = actions.iter().any(|a| matches!(a, PreflightAction::SuggestSign { .. }));
        let has_tool_calls = !response.tool_calls.is_empty();

        // FM-15 v2.2 (S2 / FR-PF-01): 先消化 ReadOnlyExplorer 的 list_directory / read_file
        // 工具调用，把 tool_result 拼到主流程的 tool_result_msgs 里，否则 LLM 收不到回执。
        let mut explorer_tool_results: Vec<serde_json::Value> = Vec::new();
        if let Some(ref ex) = explorer {
            for tc in &response.tool_calls {
                if matches!(tc.name.as_str(), "list_directory" | "read_file") {
                    let output = ex.execute(&tc.name, &tc.arguments).await
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
        let process_result = db.with_conn(|conn| {
            let mut belief_state = load_belief_state(conn, session_id);
            if iteration == 0 {
                belief_state.increment_round();
            }

            let mut tool_results = process_tool_actions(
                conn, mission_id, &actions, &mut belief_state, app, session_id,
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

        // Store assistant message + tool results in conversation history
        let assistant_msg = build_assistant_stored_msg(&response);
        stored_msgs.push(assistant_msg);
        stored_msgs.extend(tool_result_msgs.clone());

        // Decide: continue the loop, or stop and return to user?
        let needs_continuation = has_tool_calls && !has_choices && !has_suggest_sign;

        if needs_continuation {
            // Append assistant message (with tool_calls) to LLM history
            let mut assistant_content = Vec::new();
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
                tool_names = response.tool_calls.iter().map(|tc| tc.name.as_str()).collect::<Vec<_>>().join(","),
                "preflight continuing with tool_results"
            );

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

        let final_response = planner::PreflightResponse {
            text: combined_text,
            choices: response.choices,
            tool_calls: response.tool_calls.clone(),
            fallback_used: response.fallback_used.clone(),
        };
        emit_done_with_belief_state(app, session_id, &final_response, &belief_state, mode);

        let tool_names: Vec<&str> = response.tool_calls.iter().map(|tc| tc.name.as_str()).collect();
        tracing::info!(
            round = belief_state.round,
            iterations = iteration + 1,
            tool_calls_count = response.tool_calls.len(),
            tool_names = tool_names.join(","),
            fallback_used = %response.fallback_used,
            convergence_score = belief_state.convergence_score,
            phase = %belief_state.phase.label(),
            "preflight round completed"
        );
        return;
    }

    // Max iterations exhausted — save what we have and stop
    tracing::warn!("preflight reached max continuation rounds ({MAX_TOOL_CONTINUATION_ROUNDS})");
    let db = app.state::<crate::db::Database>();
    let _ = db.with_conn(|conn| {
        conn.execute(
            "UPDATE preflight_sessions SET messages = ?, updated_at = datetime('now') WHERE id = ?",
            rusqlite::params![serde_json::to_string(stored_msgs).unwrap_or_default(), session_id],
        )?;
        Ok::<(), anyhow::Error>(())
    });

    let belief_state = db
        .with_conn(|conn| Ok::<_, anyhow::Error>(load_belief_state(conn, session_id)))
        .unwrap_or_default();
    let final_response = planner::PreflightResponse {
        text: combined_text,
        choices: vec![],
        tool_calls: vec![],
        fallback_used: "none".into(),
    };
    emit_done_with_belief_state(app, session_id, &final_response, &belief_state, mode);
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
        let user_text: String = initial_message.content.iter().filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        }).collect();
        let mut stored_msgs = vec![json!({
            "role": "user",
            "content": user_text,
            "choices": []
        })];

        planner::emit_preflight_event_pub(&app_clone, &sid, "start", "");

        preflight_with_continuation(
            provider, &model, "scenario_walk",
            vec![initial_message],
            &mut stored_msgs,
            &sid, &mid, &app_clone,
        ).await;
    });

    Ok(StartPreflightResponse {
        mission_id,
        session_id,
    })
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

    if stored_msgs.len() >= 100 {
        return Err("Conversation limit reached (50 rounds). Please sign the contract.".into());
    }

    stored_msgs.push(json!({
        "role": "user",
        "content": request.message,
        "choices": []
    }));

    let history = reconstruct_history(&stored_msgs);

    let sid = request.session_id.clone();
    let mode = request.mode.clone();
    let mid = _mission_id.clone();

    let app_clone = app.clone();
    tokio::spawn(async move {
        planner::emit_preflight_event_pub(&app_clone, &sid, "start", "");

        preflight_with_continuation(
            provider, &model, &mode,
            history,
            &mut stored_msgs,
            &sid, &mid, &app_clone,
        ).await;
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

        let sql = format!("UPDATE mission_contracts SET {} WHERE id = ?", sets.join(", "));
        params.push(Box::new(contract_id));

        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();
        conn.execute(&sql, param_refs.as_slice())?;

        Ok(())
    })
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_contract(
    app: tauri::AppHandle,
    mission_id: String,
) -> Result<ContractInfo, String> {
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

                let messages: Vec<PreflightMessageInfo> = stored
                    .iter()
                    .map(|m| {
                        let choices: Vec<PreflightChoice> = m["choices"]
                            .as_array()
                            .map(|arr| {
                                arr.iter()
                                    .filter_map(|v| serde_json::from_value(v.clone()).ok())
                                    .collect()
                            })
                            .unwrap_or_default();

                        PreflightMessageInfo {
                            role: m["role"].as_str().unwrap_or("user").to_string(),
                            content: m["content"].as_str().unwrap_or("").to_string(),
                            choices,
                        }
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

    // FM-15 v2.2 (S2 / FR-PF-02): 第 1 步 —— 把合同签字 + 校验 + 拿元数据
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

            conn.execute(
                "UPDATE mission_contracts SET status = 'signed', signed_at = datetime('now'),
                 updated_at = datetime('now') WHERE id = ?",
                [&cid],
            )?;

            let (description, repo_path): (String, Option<String>) = conn.query_row(
                "SELECT description, repo_path FROM missions WHERE id = ?",
                [&mission_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            let repo_path = repo_path.ok_or_else(|| anyhow::anyhow!(
                "Mission has no repo_path; create it via FR-18 create_mission first"
            ))?;
            Ok((cid, description, repo_path))
        })
        .map_err(|e| e.to_string())?;

    let repo_path_buf = std::path::PathBuf::from(&repo_path);
    if !repo_path_buf.is_dir() {
        return Err(format!("Mission repo_path '{repo_path}' is not a directory"));
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
    let planner_output = outcome.output;
    let planner_session_id = outcome.session_id.clone();

    // 第 3 步 —— 落库 tasks + 升 mission 到 planned + 根任务 ready
    let tasks = db
        .with_conn(|conn| {
            conn.execute(
                "UPDATE missions SET title = ?, status = 'planned', updated_at = datetime('now')
                 WHERE id = ?",
                rusqlite::params![planner_output.mission_title, mission_id],
            )?;

            // 重 sign 时清掉旧 tasks（防止重复 sign 出现脏数据）
            conn.execute(
                "DELETE FROM task_dependencies WHERE task_id IN
                    (SELECT id FROM tasks WHERE mission_id = ?)",
                [&mission_id],
            )?;
            conn.execute("DELETE FROM tasks WHERE mission_id = ?", [&mission_id])?;

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

                conn.execute(
                    "INSERT INTO tasks (id, mission_id, title, description, complexity, status, role, expected_output,
                        additional_skills, consumes_artifacts, produces_artifacts, file_scope_hints)
                     VALUES (?, ?, ?, ?, ?, 'pending', ?, ?, ?, ?, ?, ?)",
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
                    ],
                )?;

                for decl in &pt.produces_artifacts {
                    crate::agent::artifacts::record_declaration(
                        conn,
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
                        conn.execute(
                            "INSERT INTO task_dependencies (task_id, depends_on, artifact_refs)
                             VALUES (?, ?, ?)",
                            rusqlite::params![task_db_id, dep_db_id, refs_json],
                        )?;
                    }
                }
            }

            // 根任务（无依赖）直接 promote 到 ready —— sign_contract 视为隐式 confirm
            conn.execute(
                "UPDATE tasks SET status = 'ready'
                 WHERE mission_id = ?1 AND status = 'pending'
                   AND id NOT IN (SELECT task_id FROM task_dependencies)",
                [&mission_id],
            )?;

            for ti in &mut task_infos {
                ti.status = conn.query_row(
                    "SELECT status FROM tasks WHERE id = ?",
                    [&ti.id],
                    |row| row.get(0),
                )?;
                ti.created_at = conn.query_row(
                    "SELECT created_at FROM tasks WHERE id = ?",
                    [&ti.id],
                    |row| row.get(0),
                )?;
            }

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
