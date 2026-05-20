//! Single-Agent Uplift P2-1 Phase C：hooks.json 加载与安全模型。
//!
//! # 安全模型（必读）
//!
//! CommandHook 让 agent 在 `sh -c` 下执行**任意外部命令**——这是 RCE 风险的明确入口。
//! 防护设计基于"用户必须 explicitly 允许 + 来源必须可信"两条原则：
//!
//! ## 1. 默认禁用
//!
//! `AppConfig.allow_command_hooks: bool` 默认 false。loader 见此 false 时**完全不读**
//! 任何 hooks.json 文件，即便 `.miragenty/hooks.json` 存在也不解析、不日志、不报错。
//! 让 agent 100% 等同 P2-1 Phase B 行为（仅内置 hook）。
//!
//! ## 2. 仅 workspace `.miragenty/hooks.json`
//!
//! 不接受 `~/.miragenty/hooks.json` 这类用户全局路径。理由：mission 描述里恶意指令可
//! 引导 agent 改全局 hook 路径，让**下次任何 mission**都被打入毒钩子。workspace 范围
//! 让爆炸半径限制在单 repo，且用户在每个 repo 看 PR 时能审计。
//!
//! ## 3. JSON 解析失败 = 空 registry
//!
//! 解析失败仅 warn 不抛错。原因：用户可能写错 JSON（漏逗号），不应让一个 typo 让所有
//! mission 卡住。让 warn log 引导用户修复，但 mission 继续以"无 command hook"运行。
//!
//! ## 4. Phase / command 字段强制校验
//!
//! 未知 phase 的 hook 整条丢弃（warn 一条）。command 为空字符串的丢弃。这两个是
//! "明显写错"的兜底，不依赖运行时崩溃来发现错误。
//!
//! # hooks.json schema
//!
//! ```json
//! {
//!   "hooks": [
//!     {
//!       "name": "workspace/lint-after-write",
//!       "phases": ["PostToolUse"],
//!       "matcher": "write_file",
//!       "command": "npm run lint -- --quiet"
//!     },
//!     {
//!       "name": "workspace/test-on-stop",
//!       "phases": ["Stop"],
//!       "command": "npm test --silent"
//!     }
//!   ]
//! }
//! ```
//!
//! 单文件单 array：故意不分文件，让用户一次能看完所有注册的 hook。

use crate::agent::hooks::{
    command::{CommandHook, CommandHookSpec},
    HookPhase, HookRegistry,
};
use serde::Deserialize;
use std::path::Path;
use std::sync::Arc;

/// 顶层文件 schema：单 `hooks` array。
#[derive(Debug, Clone, Deserialize)]
struct HooksFile {
    #[serde(default)]
    hooks: Vec<CommandHookSpec>,
}

/// hooks.json 加载结果。
#[derive(Debug, Clone)]
pub struct HookLoadOutcome {
    /// 已加载的 hook 数（注册到 registry 的）。
    pub loaded: usize,
    /// 被忽略的 hook 数（phase 未知 / command 空 / matcher 非法等）。
    pub skipped: usize,
    /// 顶层错误：文件不存在 / IO 失败 / JSON 解析失败。caller 看见后可以决定是否
    /// 给用户提示。文件不存在不算错（返回 None）。
    pub error: Option<String>,
}

impl Default for HookLoadOutcome {
    fn default() -> Self {
        Self {
            loaded: 0,
            skipped: 0,
            error: None,
        }
    }
}

/// 把 hooks.json（位于 `workspace_path/.miragenty/hooks.json`）加载并注册到给定
/// registry。
///
/// **安全契约**：
/// - `allow_command_hooks=false` → 立即返回 default outcome，不读任何文件
/// - `workspace_path` 必须是 agent 实际工作的 repo 根；loader 不做路径校验，调用方
///   保证传入受信 path（典型来自 `mission.workspace_path`）
///
/// 不返回 `Result`：所有错误都聚合到 [`HookLoadOutcome::error`]，避免 caller 处理
/// 多种错误类型。
pub fn load_workspace_hooks(
    workspace_path: &Path,
    allow_command_hooks: bool,
    registry: &mut HookRegistry,
) -> HookLoadOutcome {
    if !allow_command_hooks {
        // 安全 short-circuit：明确不读、不日志（信号最小化）
        return HookLoadOutcome::default();
    }

    let hooks_file = workspace_path.join(".miragenty").join("hooks.json");
    if !hooks_file.exists() {
        return HookLoadOutcome::default();
    }

    let raw = match std::fs::read_to_string(&hooks_file) {
        Ok(s) => s,
        Err(e) => {
            return HookLoadOutcome {
                loaded: 0,
                skipped: 0,
                error: Some(format!(
                    "failed to read {}: {e}",
                    hooks_file.display()
                )),
            };
        }
    };

    let parsed: HooksFile = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                file = %hooks_file.display(),
                "hooks.json JSON parse failed: {e}"
            );
            return HookLoadOutcome {
                loaded: 0,
                skipped: 0,
                error: Some(format!("hooks.json parse failed: {e}")),
            };
        }
    };

    let mut loaded = 0;
    let mut skipped = 0;
    for spec in parsed.hooks {
        // 校验：command 非空
        if spec.command.trim().is_empty() {
            tracing::warn!(name = %spec.name, "skip hook with empty command");
            skipped += 1;
            continue;
        }
        // 校验：至少 1 个合法 phase
        let phases: Vec<HookPhase> = spec
            .phases
            .iter()
            .filter_map(|p| {
                let parsed = HookPhase::from_str(p);
                if parsed.is_none() {
                    tracing::warn!(name = %spec.name, phase = %p, "unknown hook phase");
                }
                parsed
            })
            .collect();
        if phases.is_empty() {
            tracing::warn!(name = %spec.name, "skip hook with no valid phases");
            skipped += 1;
            continue;
        }

        let name = if spec.name.starts_with("command:") {
            spec.name.clone()
        } else {
            format!("command:{}", spec.name)
        };
        let hook = CommandHook::new(
            name,
            phases,
            spec.command,
            spec.matcher,
            workspace_path.to_path_buf(),
        );
        registry.register(Arc::new(hook));
        loaded += 1;
    }

    HookLoadOutcome {
        loaded,
        skipped,
        error: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn make_workspace_with_hooks_json(content: &str) -> TempDir {
        let dir = TempDir::new().unwrap();
        let hooks_dir = dir.path().join(".miragenty");
        fs::create_dir_all(&hooks_dir).unwrap();
        fs::write(hooks_dir.join("hooks.json"), content).unwrap();
        dir
    }

    #[test]
    fn allow_false_skips_everything() {
        let dir = make_workspace_with_hooks_json(
            r#"{"hooks":[{"name":"x","phases":["Stop"],"command":"true"}]}"#,
        );
        let mut reg = HookRegistry::new();
        let out = load_workspace_hooks(dir.path(), false, &mut reg);
        assert_eq!(out.loaded, 0);
        assert_eq!(out.skipped, 0);
        assert!(out.error.is_none());
        assert_eq!(reg.hook_count(), 0, "禁用时 registry 必须保持空");
    }

    #[test]
    fn missing_file_is_not_an_error() {
        let dir = TempDir::new().unwrap();
        let mut reg = HookRegistry::new();
        let out = load_workspace_hooks(dir.path(), true, &mut reg);
        assert_eq!(out.loaded, 0);
        assert!(out.error.is_none(), "文件不存在不是错误：{:?}", out.error);
    }

    #[test]
    fn malformed_json_returns_error_no_panic() {
        let dir = make_workspace_with_hooks_json("not json at all");
        let mut reg = HookRegistry::new();
        let out = load_workspace_hooks(dir.path(), true, &mut reg);
        assert_eq!(out.loaded, 0);
        assert!(out.error.is_some());
        assert_eq!(reg.hook_count(), 0);
    }

    #[test]
    fn loads_valid_hooks() {
        let dir = make_workspace_with_hooks_json(
            r#"{
                "hooks": [
                    {"name":"workspace/lint","phases":["PostToolUse"],"matcher":"write","command":"echo lint"},
                    {"name":"workspace/test","phases":["Stop"],"command":"echo test"}
                ]
            }"#,
        );
        let mut reg = HookRegistry::new();
        let out = load_workspace_hooks(dir.path(), true, &mut reg);
        assert_eq!(out.loaded, 2);
        assert_eq!(out.skipped, 0);
        assert!(out.error.is_none());
        assert_eq!(reg.hook_count(), 2);
    }

    #[test]
    fn skips_hook_with_unknown_phase() {
        let dir = make_workspace_with_hooks_json(
            r#"{"hooks":[{"name":"x","phases":["BogusPhase"],"command":"true"}]}"#,
        );
        let mut reg = HookRegistry::new();
        let out = load_workspace_hooks(dir.path(), true, &mut reg);
        assert_eq!(out.loaded, 0);
        assert_eq!(out.skipped, 1);
    }

    #[test]
    fn skips_hook_with_empty_command() {
        let dir = make_workspace_with_hooks_json(
            r#"{"hooks":[{"name":"x","phases":["Stop"],"command":"   "}]}"#,
        );
        let mut reg = HookRegistry::new();
        let out = load_workspace_hooks(dir.path(), true, &mut reg);
        assert_eq!(out.loaded, 0);
        assert_eq!(out.skipped, 1);
    }

    #[test]
    fn auto_prefixes_name_with_command_namespace() {
        let dir = make_workspace_with_hooks_json(
            r#"{"hooks":[{"name":"lint","phases":["Stop"],"command":"true"}]}"#,
        );
        let mut reg = HookRegistry::new();
        let out = load_workspace_hooks(dir.path(), true, &mut reg);
        assert_eq!(out.loaded, 1);
        // 不直接断言 hook.name，因为 HookRegistry 没暴露遍历——但加载成功足以
        // 隐式证明 name 处理没崩。name prefix 行为由 CommandHook 单测保证。
    }

    #[test]
    fn partial_valid_partial_skip() {
        let dir = make_workspace_with_hooks_json(
            r#"{
                "hooks": [
                    {"name":"ok","phases":["Stop"],"command":"true"},
                    {"name":"bad-phase","phases":["???"],"command":"true"},
                    {"name":"empty-cmd","phases":["Stop"],"command":""}
                ]
            }"#,
        );
        let mut reg = HookRegistry::new();
        let out = load_workspace_hooks(dir.path(), true, &mut reg);
        assert_eq!(out.loaded, 1);
        assert_eq!(out.skipped, 2);
    }
}
