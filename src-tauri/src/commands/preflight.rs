use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;
use tauri::Manager;
use uuid::Uuid;

use crate::agent::belief_state::{self, PreflightBeliefState, SlotStatus};
use crate::agent::planner::{self, ContractData, PreflightAction, PreflightChoice};
use crate::commands::mission::{build_provider, PlanMissionResponse, TaskInfo};
use crate::llm::{ContentBlock, Message, MessageRole};

// ---------- request / response types ----------

#[derive(Debug, Deserialize)]
pub struct StartPreflightRequest {
    pub description: String,
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

/// Process tool_call actions: execute DB side effects, update belief_state,
/// return tool_result messages for conversation history.
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
                        // Update belief state
                        let slot_status = match args.confidence.as_str() {
                            "confirmed" => SlotStatus::Confirmed,
                            _ => SlotStatus::Tentative,
                        };
                        if let Some(slot_id) = belief_state::map_contract_item_to_slot(&args.section, &args.item) {
                            belief_state.update_slot(slot_id, slot_status, Some(args.item.clone()), belief_state.round);
                        }

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
                let result = update_contract_item_internal(conn, &args.item_id, &args.new_content);
                match result {
                    Ok(()) => {
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
                });
            }
            _ => {
                let text = m["content"].as_str().unwrap_or("").to_string();
                history.push(Message {
                    role: MessageRole::User,
                    content: vec![ContentBlock::Text { text }],
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

    for iteration in 0..MAX_TOOL_CONTINUATION_ROUNDS {
        let response = match planner::preflight_chat(
            provider.clone(), model, mode, history.clone(), session_id, app,
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

        // Process tool_call side effects
        let db = app.state::<crate::db::Database>();
        let process_result = db.with_conn(|conn| {
            let mut belief_state = load_belief_state(conn, session_id);
            if iteration == 0 {
                belief_state.increment_round();
            }

            let tool_results = process_tool_actions(
                conn, mission_id, &actions, &mut belief_state, app, session_id,
            );

            belief_state.compute_convergence_score();
            belief_state.update_phase();
            save_belief_state(conn, session_id, &belief_state)?;

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

    let mission_id = Uuid::new_v4().to_string();
    let session_id = Uuid::new_v4().to_string();

    db.with_conn(|conn| {
        conn.execute(
            "INSERT INTO missions (id, title, description, status) VALUES (?, ?, ?, 'preflight')",
            rusqlite::params![mission_id, "Pre-flight", request.description],
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
                request.description
            ),
        }],
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

#[tauri::command]
pub async fn sign_contract(
    app: tauri::AppHandle,
    mission_id: String,
) -> Result<PlanMissionResponse, String> {
    let (provider, model) = build_provider(&app)?;
    let db = app.state::<crate::db::Database>();

    let contract_data = db
        .with_conn(|conn| {
            let contract_id = get_or_create_contract(conn, &mission_id)?;

            let scope_count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM contract_items WHERE contract_id = ? AND section = 'scope'",
                [&contract_id],
                |row| row.get(0),
            )?;

            if scope_count == 0 {
                anyhow::bail!("Cannot sign contract: at least one Scope item is required");
            }

            conn.execute(
                "UPDATE mission_contracts SET status = 'signed', signed_at = datetime('now'), updated_at = datetime('now') WHERE id = ?",
                [&contract_id],
            )?;

            let mut stmt = conn.prepare(
                "SELECT section, text FROM contract_items WHERE contract_id = ? ORDER BY created_at ASC",
            )?;

            let mut scope = vec![];
            let mut constraints = vec![];
            let mut exclusions = vec![];
            let mut assumptions = vec![];

            let rows = stmt.query_map([&contract_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;

            for row in rows {
                let (section, text) = row?;
                match section.as_str() {
                    "scope" => scope.push(text),
                    "constraints" => constraints.push(text),
                    "exclusions" => exclusions.push(text),
                    "assumptions" => assumptions.push(text),
                    _ => {}
                }
            }

            let (budget, qt, dur): (Option<f64>, Option<f64>, Option<f64>) = conn.query_row(
                "SELECT budget_usd, quality_threshold, max_duration_hours FROM mission_contracts WHERE id = ?",
                [&contract_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;

            let description: String = conn.query_row(
                "SELECT description FROM missions WHERE id = ?",
                [&mission_id],
                |row| row.get(0),
            )?;

            Ok((ContractData {
                scope,
                constraints,
                exclusions,
                assumptions,
                budget_usd: budget,
                quality_threshold: qt,
                max_duration_hours: dur,
            }, description))
        })
        .map_err(|e| e.to_string())?;

    let (contract, description) = contract_data;
    let system_prompt = planner::build_contract_aware_planner_prompt(&contract);

    let messages = vec![crate::llm::Message {
        role: MessageRole::User,
        content: vec![ContentBlock::Text {
            text: description.clone(),
        }],
    }];

    let request = crate::llm::LlmRequest {
        model: model.clone(),
        system: Some(system_prompt),
        messages,
        tools: vec![],
        max_tokens: 4096,
    };

    tracing::info!("Contract-aware planner: calling LLM model={model}");

    let text = tokio::time::timeout(
        std::time::Duration::from_secs(90),
        stream_planner_call_for_contract(provider.clone(), &request, &app),
    )
    .await
    .map_err(|_| "Planning timed out".to_string())?
    .map_err(|e| e.to_string())?;

    let planner_output = planner::parse_and_validate(&text).map_err(|e| e.to_string())?;

    let tasks = db
        .with_conn(|conn| {
            conn.execute(
                "UPDATE missions SET title = ?, status = 'planned', updated_at = datetime('now') WHERE id = ?",
                rusqlite::params![planner_output.mission_title, mission_id],
            )?;

            let mut task_infos = Vec::new();
            let mut planner_id_to_db_id: std::collections::HashMap<String, String> =
                std::collections::HashMap::new();

            for pt in &planner_output.tasks {
                let task_id = Uuid::new_v4().to_string();
                planner_id_to_db_id.insert(pt.id.clone(), task_id.clone());

                conn.execute(
                    "INSERT INTO tasks (id, mission_id, title, description, complexity, status)
                     VALUES (?, ?, ?, ?, ?, 'pending')",
                    rusqlite::params![task_id, mission_id, pt.title, pt.description, pt.complexity],
                )?;

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
                });
            }

            for pt in &planner_output.tasks {
                let task_db_id = &planner_id_to_db_id[&pt.id];
                for dep_planner_id in &pt.depends_on {
                    if let Some(dep_db_id) = planner_id_to_db_id.get(dep_planner_id) {
                        conn.execute(
                            "INSERT INTO task_dependencies (task_id, depends_on) VALUES (?, ?)",
                            rusqlite::params![task_db_id, dep_db_id],
                        )?;
                    }
                }
            }

            for ti in &mut task_infos {
                ti.created_at = conn.query_row(
                    "SELECT created_at FROM tasks WHERE id = ?",
                    [&ti.id],
                    |row| row.get(0),
                )?;
            }

            Ok(task_infos)
        })
        .map_err(|e| e.to_string())?;

    planner::emit_planner_event_pub(&app, "done", "");

    Ok(PlanMissionResponse {
        mission_id,
        tasks,
    })
}

async fn stream_planner_call_for_contract(
    provider: Arc<dyn crate::llm::LlmProvider>,
    request: &crate::llm::LlmRequest,
    app: &tauri::AppHandle,
) -> Result<String, String> {
    use crate::llm::{StreamChunk, StreamChunkKind};
    use tokio::sync::mpsc;

    let (tx, mut rx) = mpsc::channel::<StreamChunk>(256);

    let provider_clone = provider.clone();
    let request_clone = crate::llm::LlmRequest {
        model: request.model.clone(),
        system: request.system.clone(),
        messages: request.messages.clone(),
        tools: request.tools.clone(),
        max_tokens: request.max_tokens,
    };

    let stream_handle = tokio::spawn(async move {
        provider_clone.stream_chat(&request_clone, tx).await
    });

    let app_clone = app.clone();
    let mut full_text = String::new();

    while let Some(chunk) = rx.recv().await {
        match chunk.kind {
            StreamChunkKind::TextDelta => {
                full_text.push_str(&chunk.content);
                planner::emit_planner_event_pub(&app_clone, "text_delta", &chunk.content);
            }
            StreamChunkKind::ReasoningDelta => {
                planner::emit_planner_event_pub(&app_clone, "reasoning_delta", &chunk.content);
            }
            _ => {}
        }
    }

    let response = tokio::time::timeout(std::time::Duration::from_secs(5), stream_handle)
        .await
        .map_err(|_| "Stream handle timed out".to_string())?
        .map_err(|e| format!("Stream task failed: {e}"))?
        .map_err(|e| e.to_string())?;

    if full_text.is_empty() {
        full_text = response
            .content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<String>();
    }

    Ok(full_text)
}
