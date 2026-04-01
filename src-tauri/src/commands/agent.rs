use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::Manager;
use uuid::Uuid;

use crate::agent::AgentEngine;
use crate::commands::ConfigManager;
use crate::llm::{AnthropicProvider, LlmProvider, OpenAICompatProvider};

#[derive(Debug, Deserialize)]
pub struct RunAgentRequest {
    pub task_description: String,
    pub workspace_path: String,
}

#[derive(Debug, Serialize, Clone)]
pub struct RunAgentResponse {
    pub agent_id: String,
    pub status: String,
}

#[tauri::command]
pub async fn run_agent(
    app: tauri::AppHandle,
    request: RunAgentRequest,
) -> Result<RunAgentResponse, String> {
    let config_mgr = app.state::<ConfigManager>();
    let config = config_mgr.get_config_snapshot();

    let provider_key = if config.api_keys.contains_key(&config.provider) {
        &config.provider
    } else {
        "default"
    };

    let api_key = config_mgr
        .get_api_key(provider_key)
        .ok_or_else(|| format!(
            "API key not configured for provider '{}'. Go to Settings to add it.",
            config.provider
        ))?;

    let provider: Arc<dyn LlmProvider> = match config.provider.as_str() {
        "anthropic" => Arc::new(AnthropicProvider::new(api_key)),
        _ => Arc::new(OpenAICompatProvider::new(api_key, config.base_url.clone())),
    };

    let agent_id = Uuid::new_v4().to_string();
    let workspace = std::path::PathBuf::from(&request.workspace_path);
    let model = config.default_model.clone();

    let engine = AgentEngine::new(provider, workspace, app.app_handle().clone());

    let id = agent_id.clone();
    let desc = request.task_description.clone();

    tokio::spawn(async move {
        let result = engine.run(&id, &desc, &model, 20).await;
        match result {
            Ok(status) => {
                tracing::info!("Agent {id} finished with status: {status:?}");
            }
            Err(e) => {
                tracing::error!("Agent {id} error: {e}");
            }
        }
    });

    Ok(RunAgentResponse {
        agent_id,
        status: "started".to_string(),
    })
}

#[tauri::command]
pub fn stop_agent(_agent_id: String) -> Result<(), String> {
    Ok(())
}
