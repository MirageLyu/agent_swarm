use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::Manager;
use uuid::Uuid;

use crate::agent::planner::{self, ContractData, PreflightChoice};
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
    tokio::spawn(async move {
        match planner::preflight_chat(
            provider,
            &model,
            "scenario_walk",
            vec![initial_message.clone()],
            &sid,
            &app_clone,
        )
        .await
        {
            Ok(response) => {
                let db = app_clone.state::<crate::db::Database>();
                let _ = db.with_conn(|conn| {
                    let user_msg = serde_json::json!({
                        "role": "user",
                        "content": initial_message.content.iter().filter_map(|b| match b {
                            ContentBlock::Text { text } => Some(text.as_str()),
                            _ => None,
                        }).collect::<String>(),
                        "choices": []
                    });
                    let assistant_msg = serde_json::json!({
                        "role": "assistant",
                        "content": response.text,
                        "choices": response.choices
                    });

                    let msgs = serde_json::json!([user_msg, assistant_msg]);
                    conn.execute(
                        "UPDATE preflight_sessions SET messages = ?, updated_at = datetime('now') WHERE id = ?",
                        rusqlite::params![msgs.to_string(), sid],
                    )?;
                    Ok::<(), anyhow::Error>(())
                });
            }
            Err(e) => {
                tracing::error!("Preflight initial chat failed: {e}");
            }
        }
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

    stored_msgs.push(serde_json::json!({
        "role": "user",
        "content": request.message,
        "choices": []
    }));

    let history: Vec<Message> = stored_msgs
        .iter()
        .map(|m| {
            let role = match m["role"].as_str().unwrap_or("user") {
                "assistant" => MessageRole::Assistant,
                _ => MessageRole::User,
            };
            let content = m["content"].as_str().unwrap_or("").to_string();
            Message {
                role,
                content: vec![ContentBlock::Text { text: content }],
            }
        })
        .collect();

    let sid = request.session_id.clone();
    let mode = request.mode.clone();

    let app_clone = app.clone();
    tokio::spawn(async move {
        match planner::preflight_chat(provider, &model, &mode, history, &sid, &app_clone).await {
            Ok(response) => {
                let db = app_clone.state::<crate::db::Database>();
                let _ = db.with_conn(|conn| {
                    stored_msgs.push(serde_json::json!({
                        "role": "assistant",
                        "content": response.text,
                        "choices": response.choices
                    }));

                    conn.execute(
                        "UPDATE preflight_sessions SET messages = ?, updated_at = datetime('now') WHERE id = ?",
                        rusqlite::params![serde_json::to_string(&stored_msgs).unwrap_or_default(), sid],
                    )?;
                    Ok::<(), anyhow::Error>(())
                });
            }
            Err(e) => {
                tracing::error!("Preflight chat failed: {e}");
                planner::emit_preflight_event_pub(&app_clone, &sid, "error", &e.to_string());
            }
        }
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
            "SELECT id, mode, messages FROM preflight_sessions WHERE mission_id = ? ORDER BY created_at DESC LIMIT 1",
            [&mission_id],
            |row| {
                let id: String = row.get(0)?;
                let mode: String = row.get(1)?;
                let messages_json: String = row.get(2)?;
                Ok((id, mode, messages_json))
            },
        );

        match result {
            Ok((id, mode, messages_json)) => {
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
