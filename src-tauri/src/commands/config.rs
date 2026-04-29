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

    // FM-14: 审批策略 —— 见 ApprovalPolicy::default 注释了解每项语义。
    #[serde(default)]
    pub approval_policy: ApprovalPolicy,

    // i18n：UI 语言。BCP 47 tag，如 "en-US" / "zh-CN"。
    // 持久化到 config.json，启动时前端拉取后调用 i18n.changeLanguage 同步。
    // 后端不直接消费这个值（所有日志/error code 始终英文），仅作为前端偏好的真源（source of truth）。
    #[serde(default = "default_language")]
    pub language: String,
}

/// FM-14: 审批策略；持久化到 config.json，前端在 Settings 里编辑。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalPolicy {
    /// pending → expired 的等待时长。
    /// 默认 600s = agent_timeout_seconds 默认 1800s 的 1/3，
    /// 单次审批不会把 agent 顶到 wall-clock。
    #[serde(default = "default_approval_timeout_seconds")]
    pub timeout_seconds: u32,

    /// 工作区根目录的相对路径前缀（POSIX 风格 `/`）。匹配命中即触发 tool 审批。
    /// 例如：`"package.json"`、`"src-tauri/tauri.conf.json"`、`".github/"`、`"node_modules/"`。
    /// 默认覆盖典型"动一次伤一次"的元数据文件。
    #[serde(default = "default_protected_paths")]
    pub protected_paths: Vec<String>,

    /// shell_exec 命令名（命令首词，小写）。命中即触发 tool 审批。
    /// 防止 LLM 在不知情的情况下做不可逆操作。
    #[serde(default = "default_destructive_commands")]
    pub destructive_commands: Vec<String>,

    /// 累计成本超过 contract.cost_budget_usd × 此比例时，触发 budget 审批。
    /// 例：0.8 → 用掉 80% 时弹一次。0 = 关闭 budget 审批。
    #[serde(default = "default_budget_warn_ratio")]
    pub budget_warn_ratio: f32,

    /// chat agent commit_main_workdir 的"软阈值行数"：超过即触发 chat_commit 审批，
    /// 不再像旧版直接拒绝（旧版硬上限 30 行仍然在，超 30 直接 reject）。
    /// 设 0 表示禁用软阈值（所有 commit 都不审批，回退到旧硬上限行为）。
    #[serde(default = "default_chat_commit_soft_lines")]
    pub chat_commit_soft_lines: u32,
}

fn default_planner_max_steps() -> u32 { 80 }
fn default_planner_timeout_seconds() -> u64 { 600 }
fn default_planner_max_fetches() -> u32 { 10 }
fn default_max_agent_steps() -> u32 { 80 }
fn default_agent_timeout_seconds() -> u64 { 1800 }
fn default_agent_step_idle_seconds() -> u64 { 60 }

fn default_approval_timeout_seconds() -> u32 { 600 }
fn default_protected_paths() -> Vec<String> {
    vec![
        "package.json".into(),
        "package-lock.json".into(),
        "pnpm-lock.yaml".into(),
        "yarn.lock".into(),
        "Cargo.toml".into(),
        "Cargo.lock".into(),
        "src-tauri/tauri.conf.json".into(),
        ".github/".into(),
        ".env".into(),
        ".env.local".into(),
    ]
}
fn default_destructive_commands() -> Vec<String> {
    vec![
        "rm".into(),
        "git push".into(),
        "git reset".into(),
        "git rebase".into(),
        "npm publish".into(),
        "pnpm publish".into(),
        "yarn publish".into(),
        "cargo publish".into(),
    ]
}
fn default_budget_warn_ratio() -> f32 { 0.8 }
fn default_chat_commit_soft_lines() -> u32 { 10 }

fn default_language() -> String { "en-US".to_string() }

impl Default for ApprovalPolicy {
    fn default() -> Self {
        Self {
            timeout_seconds: default_approval_timeout_seconds(),
            protected_paths: default_protected_paths(),
            destructive_commands: default_destructive_commands(),
            budget_warn_ratio: default_budget_warn_ratio(),
            chat_commit_soft_lines: default_chat_commit_soft_lines(),
        }
    }
}

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
            approval_policy: ApprovalPolicy::default(),
            language: default_language(),
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
    /// i18n: BCP 47 tag, e.g. "en-US" / "zh-CN"
    pub language: String,
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
        language: config.language.clone(),
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
    /// i18n: BCP 47 tag。前端传入后立即调用 i18n.changeLanguage 同步。
    pub language: Option<String>,
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
        if let Some(lang) = request.language {
            // 仅接受白名单内的 BCP 47 tag，避免脏数据。
            // 后续支持新语言时在这里追加，且需提供对应 locale json。
            const SUPPORTED: &[&str] = &["en-US", "zh-CN"];
            let trimmed = lang.trim();
            if SUPPORTED.iter().any(|s| s.eq_ignore_ascii_case(trimmed)) {
                // 规范化大小写（en-US 而非 en-us）
                let canonical = SUPPORTED
                    .iter()
                    .find(|s| s.eq_ignore_ascii_case(trimmed))
                    .unwrap();
                config.language = (*canonical).to_string();
            }
        }
    }
    mgr.save().map_err(|e| e.to_string())
}

// ---- FM-14: Approval Policy IPC ----

#[tauri::command]
pub fn get_approval_policy(app: tauri::AppHandle) -> Result<ApprovalPolicy, String> {
    let mgr = app.state::<ConfigManager>();
    let policy = mgr.config.lock().unwrap().approval_policy.clone();
    Ok(policy)
}

#[derive(Debug, Deserialize)]
pub struct UpdateApprovalPolicyRequest {
    pub timeout_seconds: Option<u32>,
    pub protected_paths: Option<Vec<String>>,
    pub destructive_commands: Option<Vec<String>>,
    pub budget_warn_ratio: Option<f32>,
    pub chat_commit_soft_lines: Option<u32>,
}

#[tauri::command]
pub fn update_approval_policy(
    app: tauri::AppHandle,
    request: UpdateApprovalPolicyRequest,
) -> Result<ApprovalPolicy, String> {
    let mgr = app.state::<ConfigManager>();
    let updated = {
        let mut config = mgr.config.lock().unwrap();
        let p = &mut config.approval_policy;
        if let Some(v) = request.timeout_seconds {
            // 30s 是用户在前端弹窗里能反应过来的最小值；上限 1h 防误填。
            p.timeout_seconds = v.clamp(30, 3600);
        }
        if let Some(v) = request.protected_paths {
            p.protected_paths = v
                .into_iter()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
        }
        if let Some(v) = request.destructive_commands {
            p.destructive_commands = v
                .into_iter()
                .map(|s| s.trim().to_lowercase())
                .filter(|s| !s.is_empty())
                .collect();
        }
        if let Some(v) = request.budget_warn_ratio {
            // 0 = 关闭；否则限制 [0.1, 1.0]
            p.budget_warn_ratio = if v <= 0.0 { 0.0 } else { v.clamp(0.1, 1.0) };
        }
        if let Some(v) = request.chat_commit_soft_lines {
            p.chat_commit_soft_lines = v;
        }
        p.clone()
    };
    mgr.save().map_err(|e| e.to_string())?;
    Ok(updated)
}
