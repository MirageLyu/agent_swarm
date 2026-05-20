use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tauri::Manager;

use crate::error_code::IpcError;
use crate::llm::{
    AnthropicProvider, ContentBlock, LlmProvider, LlmRequest, Message, MessageRole,
    OpenAICompatProvider, TokenUsage,
};

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

    /// 主 LLM 请求 `max_tokens`（每步 output 上限）。
    ///
    /// 老版本写死 4096，对一次性生成大段 markdown / 多文件方案的 agent 步骤来说不够——
    /// LLM 会在 tool_use 的 JSON 字符串里被截断，stream 给到我们的 arguments 是残缺 JSON，
    /// 解析失败 → tool 调用失败 → LLM 重试同样大 output → 循环 → 看起来像"卡住"。
    /// 16384 在 99% 场景够用且绝大多数 provider/model（Claude / DeepSeek-V4 / Qwen3）支持。
    /// 用户在 settings 可改：DeepSeek-V4 上限 384K，Claude Sonnet 64K，本地模型按部署调。
    #[serde(default = "default_agent_max_output_tokens")]
    pub agent_max_output_tokens: u32,

    /// LLM stream "建立连接前 / 收到首 chunk 前" 的网络错误重试次数。
    /// 0 = 不重试（旧行为）。5 次（指数退避 1s/2s/4s/8s/16s 累计 31s）足够扛过
    /// 地铁/电梯/WiFi 切流量这类持续 30s 内的网络黑洞。
    /// 注意：一旦收到首个 chunk 还断网，**不重试**——重试会让用户看到重复内容；
    /// 走 stream_chat 内部 partial-fallback（保留已收到内容当成功结束）。
    #[serde(default = "default_stream_network_retries")]
    pub stream_network_retries: u32,
    /// 网络重试的首次退避毫秒数。指数退避：N、2N、4N、8N...
    #[serde(default = "default_stream_initial_retry_delay_ms")]
    pub stream_initial_retry_delay_ms: u64,

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

    // ---- Single-Agent Uplift B2: tool_summary 小模型配置 ----
    // 当 tool_result 字符数超过阈值时，引擎调一个独立的小模型（默认 deepseek-v4-flash）
    // 把内容压成 ≤500 字结构化摘要塞回 messages，省 context。
    // 缺省/留空 = 关闭摘要，回退到旧版"截尾保留 1KB"行为，安全等价。
    /// 摘要小模型名。空字符串视为关闭。Defaults to deepseek-v4-flash（2026-04 发布）。
    #[serde(default = "default_tool_summary_model")]
    pub tool_summary_model: String,
    /// 摘要小模型的 OpenAI-compat base URL。和 default base_url 解耦，
    /// 因为多数用户的主模型走 dashscope，但 deepseek-v4-flash 在 deepseek 自家 endpoint 才有。
    #[serde(default = "default_tool_summary_base_url")]
    pub tool_summary_base_url: String,
    /// 摘要 provider 类型。"openai_compat" / "anthropic"。当前所有 deepseek 走前者。
    #[serde(default = "default_tool_summary_provider")]
    pub tool_summary_provider: String,
    /// 触发摘要的字符阈值。低于此值直接放行，高于此值才走摘要 → truncate fallback。
    #[serde(default = "default_tool_summary_threshold_chars")]
    pub tool_summary_threshold_chars: u32,

    /// **Evaluator agent 的 wall-clock 超时**（秒）。
    ///
    /// 历史上这条值写死在 scheduler.rs 的 `spawn_evaluator` 里 = 30s，
    /// 对 reasoning 模型（`deepseek-v4-pro` / `glm-5` 等）+ 大 diff 输入根本不够：
    /// dd68c400 案例里 33KB 设计文档 + 17 条 acceptance criteria 的 review，
    /// 30s 时模型还没开始吐 final content，被强杀。
    ///
    /// 改成可配置后默认 **600s（10 min）**——和主 agent 的 step idle watchdog
    /// 配合 (`stream_chat_with_idle_guard` 默认 120s idle)，正常 review 1-3 分钟
    /// 跑完，wall-clock 仅作为兜底防御 LLM 死循环。
    #[serde(default = "default_evaluator_timeout_seconds")]
    pub evaluator_timeout_seconds: u64,

    // ---- Single-Agent Uplift P0-2: Output token budget + diminishing-returns ----
    /// 单 agent 在所有 step 累计输出 token 的软上限。
    ///
    /// 当 agent 用掉 90% 或连续 3 轮新输出 < 500 token（边际收益递减）时，
    /// 引擎注入"该收尾了"的提示促 agent 调 task_complete。`max_steps` 作为
    /// 硬上限兜底保留。0 = 关闭预算控制，回退到 max_steps-only 旧行为（安全等价）。
    ///
    /// 推荐值：模型 context window 的约 30%（128K 模型 → 40000）。
    #[serde(default = "default_agent_output_token_budget")]
    pub agent_output_token_budget: u64,

    // ---- Single-Agent Uplift P1-2: Cross-model fallback ----
    /// Fallback 模型名。主模型返回过载（5xx）/ 限速（429）时自动切换重发。
    ///
    /// 空字符串 = 关闭（行为同旧版）。必须是当前 provider 能调起的模型——
    /// **跨厂商 fallback 尚不支持**（API key / endpoint 不同），后续 P1-2 Phase D。
    #[serde(default)]
    pub agent_fallback_model: String,

    /// 粘性 fallback：切换后是否继续用 fallback 模型（默认 true）。
    ///
    /// - true（默认）：切换后下一 step 仍用 fallback。上游过载通常持续几分钟，
    ///   频繁切回主模型 = 反复撞墙。
    /// - false：下一 step 重新尝试主模型；若仍失败会再次切换。适合"主模型偶发
    ///   一次性 5xx"场景，但成本是每次都要重试一次主模型。
    #[serde(default = "default_agent_fallback_sticky")]
    pub agent_fallback_sticky: bool,

    // ---- Single-Agent Uplift P2-1 Phase C: Command Hook 启用开关 ----
    /// 是否允许加载 `<workspace>/.miragenty/hooks.json` 中定义的 CommandHook。
    ///
    /// **默认 false**——CommandHook 让 agent 经 `sh -c` 执行任意命令，是 RCE 入口。
    /// 用户必须 explicit 在 Settings → Developer 打开此开关。
    ///
    /// 开启后仍受三层防护：
    /// 1. 只读 workspace 内 `.miragenty/hooks.json`（不接受 user-global 路径，
    ///    避免恶意 mission 指令引导改全局配置）
    /// 2. 每个 command hook 60s timeout
    /// 3. stdout JSON 解析失败 / 非 0 退出 → 退化为 InjectMessage(warning)，
    ///    **不**让一个 hook 错误导致整 agent fail
    ///
    /// 详见 [`crate::agent::hooks::config`] 模块文档。
    #[serde(default)]
    pub allow_command_hooks: bool,
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
fn default_agent_max_output_tokens() -> u32 { 16384 }
fn default_stream_network_retries() -> u32 { 5 }
fn default_stream_initial_retry_delay_ms() -> u64 { 1000 }

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

/// 默认 tool_summary 小模型：`deepseek-v4-flash`。
///
/// # 与 reasoning 模式的关系
///
/// V4 系列是 reasoning 模型，**裸调用** max_tokens=600 时 reasoning_tokens 会
/// 吞光所有预算，`content` 永远空。但 [`crate::llm::deepseek_adapter`] 已经
/// 在 [`crate::agent::tool_summarizer::ToolSummarizer`] 内部自动注入
/// `thinking: {"type": "disabled"}`——dial-test 验证（2026-05-18）：
///
/// | 模型 | 时延 | content | 备注 |
/// |---|---|---|---|
/// | v4-flash 裸 | ~7s | 含 reasoning 拖累 | 不可用 |
/// | **v4-flash + thinking:disabled** | **1.5s** | 详细 | 当前默认 |
/// | v3-2-251201 | 2.3s | 略简 | 备选 |
///
/// 即 v4-flash 在适配下**比 v3-2 快 35%、内容更详细**（更易抓到关键调用名/路由）。
///
/// 用户在 settings 里可以换成任何非 reasoning 小模型；reseller 切换或
/// 模型升级出问题时，[`crate::commands::test_tool_summarizer_connection`]
/// 命令可以一键 dial-test 看 health_check 结果。
fn default_tool_summary_model() -> String { "deepseek-v4-flash".to_string() }
/// 默认 base_url 跟随主 LLM 的 reseller（默认 bitfun）。原先写死
/// `api.deepseek.com` 配 reseller key 必然 401（生产 ****zvdf 案例）。
/// 用户实际部署里"主模型 reseller + summary 走官方"是少数派，让默认值
/// 跟主 base_url 一致更不易出错。
fn default_tool_summary_base_url() -> String { "https://api.openbitfun.com/v1".to_string() }
fn default_tool_summary_provider() -> String { "openai_compat".to_string() }
fn default_tool_summary_threshold_chars() -> u32 { 8 * 1024 }
fn default_evaluator_timeout_seconds() -> u64 { 600 }

// Single-Agent Uplift P0-2 / P1-2 默认值。
//
// P0-2: 0 = 关闭。我们**默认关闭**预算控制原因——max_steps 已经是兜底，
// 且不少用户依赖"agent 跑满 80 step 才停"的行为；预算控制属于 opt-in 优化。
// Settings UI 里会推荐"约 context window 的 30%"作为典型起点。
fn default_agent_output_token_budget() -> u64 { 0 }
// P1-2: 默认 sticky=true。详见字段注释。
fn default_agent_fallback_sticky() -> bool { true }

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
            agent_max_output_tokens: default_agent_max_output_tokens(),
            stream_network_retries: default_stream_network_retries(),
            stream_initial_retry_delay_ms: default_stream_initial_retry_delay_ms(),
            approval_policy: ApprovalPolicy::default(),
            language: default_language(),
            tool_summary_model: default_tool_summary_model(),
            tool_summary_base_url: default_tool_summary_base_url(),
            tool_summary_provider: default_tool_summary_provider(),
            tool_summary_threshold_chars: default_tool_summary_threshold_chars(),
            evaluator_timeout_seconds: default_evaluator_timeout_seconds(),
            agent_output_token_budget: default_agent_output_token_budget(),
            agent_fallback_model: String::new(),
            agent_fallback_sticky: default_agent_fallback_sticky(),
            allow_command_hooks: false,
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
    // Single-Agent Uplift P0-2 / P1-2
    pub agent_output_token_budget: u64,
    pub agent_fallback_model: String,
    pub agent_fallback_sticky: bool,
    // Single-Agent Uplift P2-1 Phase C
    pub allow_command_hooks: bool,
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
        agent_output_token_budget: config.agent_output_token_budget,
        agent_fallback_model: config.agent_fallback_model.clone(),
        agent_fallback_sticky: config.agent_fallback_sticky,
        allow_command_hooks: config.allow_command_hooks,
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
    // Single-Agent Uplift P0-2 / P1-2
    pub agent_output_token_budget: Option<u64>,
    pub agent_fallback_model: Option<String>,
    pub agent_fallback_sticky: Option<bool>,
    // Single-Agent Uplift P2-1 Phase C
    pub allow_command_hooks: Option<bool>,
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
        if let Some(v) = request.agent_output_token_budget {
            // 上限 1M 对绝大多数 long-context 模型够用（Gemini 2M 是异类，且 1M
            // tokens 的 output 已经远超合理 agent 单次任务规模）。
            // 防误填 1e18 把 i64 溢成负数。
            config.agent_output_token_budget = v.min(1_000_000);
        }
        if let Some(v) = request.agent_fallback_model {
            // trim 防 UI 残留空白。空串保留语义 = 关闭。
            config.agent_fallback_model = v.trim().to_string();
        }
        if let Some(v) = request.agent_fallback_sticky {
            config.agent_fallback_sticky = v;
        }
        if let Some(v) = request.allow_command_hooks {
            // 关键安全决策：用户必须 explicit set true 才启用。tracing 记录开关变更
            // 让 audit log 有迹可循（哪个 mission 之前一刻被打开）。
            tracing::info!(
                allow_command_hooks_was = config.allow_command_hooks,
                allow_command_hooks_now = v,
                "AppConfig.allow_command_hooks toggled"
            );
            config.allow_command_hooks = v;
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

// ---- 模型连通性测试 ----
//
// 用户在 Settings 里改 provider/base_url/model 后想"先验证再保存"。
// 设计：
// - 入参全部 Optional，缺省回退到已保存的 config 值；这样未保存的 form 也能预试
// - API key 永远从已保存的 config 读取（按 effective provider 路由），不在 IPC 上传敏感串
// - 走非流式 chat，max_tokens=10，prompt 极短，把成本和延迟都控在最小
// - 失败用 IpcError 走 i18n（前端 errors namespace）

#[derive(Debug, Deserialize)]
pub struct TestLlmConnectionRequest {
    pub provider: Option<String>,
    pub base_url: Option<String>,
    pub model: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct TestLlmConnectionResponse {
    pub provider: String,
    pub model: String,
    pub latency_ms: u64,
    pub sample_text: String,
    pub usage: TokenUsage,
}

#[tauri::command]
pub async fn test_llm_connection(
    app: tauri::AppHandle,
    request: TestLlmConnectionRequest,
) -> Result<TestLlmConnectionResponse, String> {
    let mgr = app.state::<ConfigManager>();
    let snapshot = mgr.get_config_snapshot();

    let provider_name = request
        .provider
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| snapshot.provider.clone());
    let base_url = request
        .base_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| snapshot.base_url.clone());
    let model = request
        .model
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| snapshot.default_model.clone());

    // API key 路由：优先按 form 里的 provider 找，找不到回退到 "default"
    let api_key = mgr
        .get_api_key(&provider_name)
        .or_else(|| mgr.get_api_key("default"))
        .ok_or_else(|| IpcError::no_api_key(provider_name.clone()).to_string())?;

    let provider: Arc<dyn LlmProvider> = match provider_name.as_str() {
        "anthropic" => Arc::new(AnthropicProvider::new(api_key)),
        _ => Arc::new(OpenAICompatProvider::new(api_key, base_url.clone())),
    };

    let req = LlmRequest {
        model: model.clone(),
        system: None,
        messages: vec![Message {
            role: MessageRole::User,
            content: vec![ContentBlock::Text {
                text: "Reply with the single word: pong".to_string(),
            }],
            cache_control: None,
        }],
        tools: vec![],
        max_tokens: 16,
        provider_extras: None,
    };

    let started = Instant::now();
    let response = provider.chat(&req).await.map_err(|e| {
        IpcError::provider_unavailable(e.to_string())
            .with_detail(format!("provider={provider_name} model={model}"))
            .to_string()
    })?;
    let latency_ms = started.elapsed().as_millis() as u64;

    // 把第一段 text 截短返回；非 text content（tool_use 等）忽略
    let sample_text = response
        .content
        .iter()
        .find_map(|c| match c {
            ContentBlock::Text { text } => Some(text.clone()),
            _ => None,
        })
        .unwrap_or_default();
    let sample_text = if sample_text.chars().count() > 200 {
        sample_text.chars().take(200).collect::<String>() + "…"
    } else {
        sample_text
    };

    Ok(TestLlmConnectionResponse {
        provider: provider_name,
        model,
        latency_ms,
        sample_text,
        usage: response.usage,
    })
}

// ---- tool_summary 配置 dial-test ----
//
// 用户改完 tool_summary_model / tool_summary_base_url 后想"先验证再保存"。
// tool_summary 比主 LLM 更挑剔——必须用**非 reasoning** 小模型，否则
// max_tokens=600 会被 reasoning_tokens 全吞掉，content 永远空。
// 主 LLM 用 reasoning model 没问题，但 tool_summary 不行。
//
// 这条命令的特殊价值：能区分"网络/认证错"与"reasoning model 误用"。

#[derive(Debug, Deserialize)]
pub struct TestToolSummarizerRequest {
    /// 模型名。缺省回退到当前 config.tool_summary_model。
    pub model: Option<String>,
    /// base URL。缺省回退到当前 config.tool_summary_base_url。
    pub base_url: Option<String>,
    /// 用于查找 api_key 的 provider 名。缺省回退到 config.tool_summary_provider。
    /// API key 永远从已保存的 config 读，不在 IPC 上传敏感串。
    pub provider: Option<String>,
}

#[tauri::command]
pub async fn test_tool_summarizer_connection(
    app: tauri::AppHandle,
    request: TestToolSummarizerRequest,
) -> Result<crate::agent::tool_summarizer::HealthOutcome, String> {
    let mgr = app.state::<ConfigManager>();
    let snapshot = mgr.get_config_snapshot();

    let model = request
        .model
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| snapshot.tool_summary_model.clone());
    let base_url = request
        .base_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| snapshot.tool_summary_base_url.clone());
    let provider_name = request
        .provider
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| snapshot.tool_summary_provider.clone());

    if model.is_empty() {
        return Err(IpcError::no_api_key(
            serde_json::Value::String("tool_summary".to_string()),
        )
        .with_detail("tool_summary_model is empty — set a non-reasoning model first")
        .to_string());
    }

    // API key fallback：保持和 agent.rs 启动 summarizer 时一致的查找链——
    // 先按 provider 名找（典型 "openai_compat"），fallback "deepseek"，再 fallback "default"。
    let api_key = mgr
        .get_api_key(&provider_name)
        .or_else(|| mgr.get_api_key("deepseek"))
        .or_else(|| mgr.get_api_key("default"))
        .ok_or_else(|| IpcError::no_api_key(provider_name.clone()).to_string())?;

    let summarizer = crate::agent::tool_summarizer::ToolSummarizer::try_openai_compat(
        api_key, base_url, model,
    )
    .ok_or_else(|| {
        IpcError::no_api_key(serde_json::Value::String("tool_summary".to_string()))
            .with_detail("ToolSummarizer construction failed (empty model or key)")
            .to_string()
    })?;

    Ok(summarizer.health_check().await)
}
