use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::Manager;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct AppConfig {
    pub api_keys: HashMap<String, String>,
    pub default_model: String,
    pub max_concurrent_agents: u32,
}

pub struct ConfigManager {
    config: Mutex<AppConfig>,
    config_path: PathBuf,
}

impl ConfigManager {
    pub fn load(data_dir: &PathBuf) -> Self {
        let config_path = data_dir.join("config.json");
        let config = if config_path.exists() {
            let data = std::fs::read_to_string(&config_path).unwrap_or_default();
            serde_json::from_str(&data).unwrap_or_default()
        } else {
            AppConfig {
                api_keys: HashMap::new(),
                default_model: "claude-sonnet-4-20250514".to_string(),
                max_concurrent_agents: 3,
            }
        };

        Self {
            config: Mutex::new(config),
            config_path,
        }
    }

    pub fn get_api_key(&self, provider: &str) -> Option<String> {
        let config = self.config.lock().unwrap();
        config.api_keys.get(provider).cloned()
    }

    fn save(&self) -> anyhow::Result<()> {
        let config = self.config.lock().unwrap();
        let data = serde_json::to_string_pretty(&*config)?;
        std::fs::write(&self.config_path, data)?;
        Ok(())
    }
}

#[derive(Debug, Serialize)]
pub struct ConfigResponse {
    pub default_model: String,
    pub max_concurrent_agents: u32,
    pub has_anthropic_key: bool,
}

#[tauri::command]
pub fn get_config(app: tauri::AppHandle) -> Result<ConfigResponse, String> {
    let mgr = app.state::<ConfigManager>();
    let config = mgr.config.lock().unwrap();
    Ok(ConfigResponse {
        default_model: config.default_model.clone(),
        max_concurrent_agents: config.max_concurrent_agents,
        has_anthropic_key: config.api_keys.contains_key("anthropic"),
    })
}

#[derive(Debug, Deserialize)]
pub struct SetApiKeyRequest {
    pub provider: String,
    pub key: String,
}

#[tauri::command]
pub fn set_api_key(app: tauri::AppHandle, request: SetApiKeyRequest) -> Result<(), String> {
    let mgr = app.state::<ConfigManager>();
    {
        let mut config = mgr.config.lock().unwrap();
        config.api_keys.insert(request.provider, request.key);
    }
    mgr.save().map_err(|e| e.to_string())
}

#[derive(Debug, Deserialize)]
pub struct UpdateConfigRequest {
    pub default_model: Option<String>,
    pub max_concurrent_agents: Option<u32>,
}

#[tauri::command]
pub fn update_config(
    app: tauri::AppHandle,
    request: UpdateConfigRequest,
) -> Result<(), String> {
    let mgr = app.state::<ConfigManager>();
    {
        let mut config = mgr.config.lock().unwrap();
        if let Some(model) = request.default_model {
            config.default_model = model;
        }
        if let Some(max) = request.max_concurrent_agents {
            config.max_concurrent_agents = max;
        }
    }
    mgr.save().map_err(|e| e.to_string())
}
