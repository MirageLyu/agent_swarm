use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::Manager;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub api_keys: HashMap<String, String>,
    pub default_model: String,
    pub base_url: String,
    pub provider: String,
    pub max_concurrent_agents: u32,

    // FM-15 v2.2 (S3-1, S3-4): Planner Agent Loop tuning + fetch_url 守卫。
    // 缺省值与 requirements.md「新增配置项」一致；旧 config.json 反序列化时会用 default。
    #[serde(default = "default_planner_max_steps")]
    pub planner_max_steps: u32,
    #[serde(default = "default_planner_timeout_seconds")]
    pub planner_timeout_seconds: u64,
    /// 永久白名单（顶级域名，如 `example.com`），匹配 `host` 末尾。
    #[serde(default)]
    pub planner_fetch_allowlist: Vec<String>,
    #[serde(default = "default_planner_max_fetches")]
    pub planner_max_fetches_per_session: u32,

    // FM-15 v2.2 (Phase 3, FR-11): Coding Agent 步数 + 超时硬上限。
    #[serde(default = "default_max_agent_steps")]
    pub max_agent_steps: u32,
    #[serde(default = "default_agent_timeout_seconds")]
    pub agent_timeout_seconds: u64,

    // FM-15 follow-up: 多层超时看门狗。
    /// LLM 流式响应"相邻 chunk 静默"上限，秒。0 = 不启用 idle 检测，仅靠全局 timeout。
    /// Provider 启动时一次性读取，改完需要重启 app（或下一次任务）才生效。
    #[serde(default = "default_agent_step_idle_seconds")]
    pub agent_step_idle_seconds: u64,
}

fn default_planner_max_steps() -> u32 { 80 }
fn default_planner_timeout_seconds() -> u64 { 600 }
fn default_planner_max_fetches() -> u32 { 10 }
fn default_max_agent_steps() -> u32 { 80 }
fn default_agent_timeout_seconds() -> u64 { 1800 }
fn default_agent_step_idle_seconds() -> u64 { 60 }

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            api_keys: HashMap::new(),
            default_model: "qwen3.5-plus".to_string(),
            base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1".to_string(),
            provider: "openai_compat".to_string(),
            max_concurrent_agents: 3,
            planner_max_steps: default_planner_max_steps(),
            planner_timeout_seconds: default_planner_timeout_seconds(),
            planner_fetch_allowlist: Vec::new(),
            planner_max_fetches_per_session: default_planner_max_fetches(),
            max_agent_steps: default_max_agent_steps(),
            agent_timeout_seconds: default_agent_timeout_seconds(),
            agent_step_idle_seconds: default_agent_step_idle_seconds(),
        }
    }
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
            AppConfig::default()
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

    pub fn get_config_snapshot(&self) -> AppConfig {
        self.config.lock().unwrap().clone()
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
    pub base_url: String,
    pub provider: String,
    pub max_concurrent_agents: u32,
    pub has_api_key: bool,
    pub max_agent_steps: u32,
    pub agent_timeout_seconds: u64,
    pub agent_step_idle_seconds: u64,
}

#[tauri::command]
pub fn get_config(app: tauri::AppHandle) -> Result<ConfigResponse, String> {
    let mgr = app.state::<ConfigManager>();
    let config = mgr.config.lock().unwrap();
    let has_key = config.api_keys.contains_key(&config.provider)
        || config.api_keys.contains_key("default");
    Ok(ConfigResponse {
        default_model: config.default_model.clone(),
        base_url: config.base_url.clone(),
        provider: config.provider.clone(),
        max_concurrent_agents: config.max_concurrent_agents,
        has_api_key: has_key,
        max_agent_steps: config.max_agent_steps,
        agent_timeout_seconds: config.agent_timeout_seconds,
        agent_step_idle_seconds: config.agent_step_idle_seconds,
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
    pub base_url: Option<String>,
    pub provider: Option<String>,
    pub max_concurrent_agents: Option<u32>,
    pub max_agent_steps: Option<u32>,
    pub agent_timeout_seconds: Option<u64>,
    pub agent_step_idle_seconds: Option<u64>,
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
        if let Some(url) = request.base_url {
            config.base_url = url;
        }
        if let Some(prov) = request.provider {
            config.provider = prov;
        }
        if let Some(max) = request.max_concurrent_agents {
            config.max_concurrent_agents = max;
        }
        if let Some(v) = request.max_agent_steps {
            config.max_agent_steps = v.max(1);
        }
        if let Some(v) = request.agent_timeout_seconds {
            config.agent_timeout_seconds = v.max(60);
        }
        if let Some(v) = request.agent_step_idle_seconds {
            // 0 = 关闭 idle 检测；否则至少 5s 防误杀。
            config.agent_step_idle_seconds = if v == 0 { 0 } else { v.max(5) };
        }
    }
    mgr.save().map_err(|e| e.to_string())
}
