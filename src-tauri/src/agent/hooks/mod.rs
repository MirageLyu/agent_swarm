//! Single-Agent Uplift P2-1：通用 Stop Hook 体系。
//!
//! # 解决的问题
//!
//! 当前 agent loop 只在 `task_complete` 时跑 `guardrail` 一类 hook（写死在
//! `engine.rs` 里），其它 phase（PreLlmCall / PostToolUse / Compact 前后 / Stop）
//! **没有任何**可插拔扩展点。用户想做"每次 write_file 后跑一次 tsc 编译检查"
//! 这种事必须改 engine.rs 或写新 guardrail，对**外部脚本扩展完全封闭**。
//!
//! 本模块抽象出通用 hook 体系：
//!   - [`HookPhase`]：7 个 agent 生命周期事件点
//!   - [`AgentHook`] trait：所有 hook（内置 / Command / 未来的 LlmEvaluator）的统一接口
//!   - [`HookOutcome`]：Pass / InjectMessage / PreventContinuation 三种结果
//!   - [`HookRegistry`]：phase + matcher 路由 + 短路语义
//!
//! # 三阶段交付（本文件是 Phase A）
//!
//! 1. **Phase A（本 PR）**：trait + registry + 单测。**0 行为变化**——
//!    engine.rs 还没接线，registry 永远空。
//! 2. **Phase B（后续 PR）**：engine.rs 7 处 phase 调用点接线 + 现有 Guardrail
//!    包装成内置 hook（行为对等迁移）。
//! 3. **Phase C（更晚）**：Command hook + workspace `.miragenty/hooks.json`
//!    加载 + Settings UI 权限 toggle。
//!
//! 拆三个 Phase 的原因：每个 Phase 都是可独立 review / rollback 的增量。
//! Phase A 落地后 trait 契约被单测锁定，Phase B 接线时只要保证 registry 行为
//! 不变即可，回归面小。
//!
//! # 设计取舍
//!
//! ## 为什么 HookContext 用 owned struct 而非 `&references`
//!
//! Hook 可能是 async + Send + Sync 的（要 spawn 到 tokio runtime）。如果 ctx
//! 借 engine 内的引用，hook future 借的生命周期一路向上拉到 engine.rs 主循环，
//! 编译错误地狱。owned 数据 + clone 一次 cost 很小（context 字段都是 short
//! 字符串 / 数字）。
//!
//! ## 为什么 phases 是 `&[HookPhase]` 而非 single phase
//!
//! 一个 hook 可能同时关心多个 phase（如某个"全程 telemetry" hook 在所有 phase
//! 都跑）。让 hook 自己声明感兴趣的 phase，registry 按 phase 过滤——比"一个 hook
//! 必须对应一个 phase" 灵活，而且 `&[]` 数组形式编译期不可变，没有运行时成本。
//!
//! ## 为什么 PreventContinuation 携带 `terminal: bool` 而非两个独立 variant
//!
//! 两种语义：
//!   - `terminal: true` → 立即 fail 整 agent（如严重安全错误）
//!   - `terminal: false` → 结束当前 step 不继续后续 hook，但 agent 仍可下一 step
//!
//! 用 bool 而非独立 variant 是因为下游主循环按这个 bool 走两条分支即可，
//! 加 variant 反而让 caller 多写一个 match arm。

use serde::{Deserialize, Serialize};
use std::sync::Arc;

// P2-1 Phase C 子模块。仅在 `AppConfig.allow_command_hooks=true` 时由
// scheduler 经 [`config::load_workspace_hooks`] 加载并注册到 registry。
pub mod command;
pub mod config;

/// Agent 生命周期中的 hook 触发点。
///
/// **添加新 phase 必须**：
///   1. 在此 enum 加变体
///   2. 在 engine.rs 主循环对应位置加 registry.execute_phase 调用（Phase B）
///   3. 给该 phase 写至少一个 integration test
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HookPhase {
    /// 每 step 发起 LLM 调用之前。用途：注入额外 system context / 预算检查
    PreLlmCall,
    /// LLM 响应到手、tool_use 解析前。用途：记录响应 / reasoning 分析
    PostSampling,
    /// 每次 tool_use 执行后。用途：tsc / lint / test 检查（最高频用例）
    PostToolUse,
    /// microcompact / reactive compact 触发前。用途：持久化要保留的关键上下文
    PreCompact,
    /// compact 完成后。用途：重新注入丢失的 context
    PostCompact,
    /// LLM 不再产 tool_use（自然 turn 结束）或调 task_complete。
    /// 用途：全套质量检查、提交前钩子
    Stop,
    /// guardrail 全部通过、status → completed 之前。
    /// 用途：publish artifact / send notification
    TaskCompleted,
}

impl HookPhase {
    /// 给日志 / event meta 用的稳定标识。
    ///
    /// **改名警告**：这些字符串会进 hook 配置文件（`.miragenty/hooks.json`），
    /// 改了等于破坏用户已有配置。新名只能加，不能改。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::PreLlmCall => "PreLlmCall",
            Self::PostSampling => "PostSampling",
            Self::PostToolUse => "PostToolUse",
            Self::PreCompact => "PreCompact",
            Self::PostCompact => "PostCompact",
            Self::Stop => "Stop",
            Self::TaskCompleted => "TaskCompleted",
        }
    }

    /// 从字符串解析（配置文件加载用）。返回 None 表示用户写了未知 phase 名。
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "PreLlmCall" => Some(Self::PreLlmCall),
            "PostSampling" => Some(Self::PostSampling),
            "PostToolUse" => Some(Self::PostToolUse),
            "PreCompact" => Some(Self::PreCompact),
            "PostCompact" => Some(Self::PostCompact),
            "Stop" => Some(Self::Stop),
            "TaskCompleted" => Some(Self::TaskCompleted),
            _ => None,
        }
    }
}

/// hook injected message 的严重程度。影响 agent 后续行为：
///   - Info：仅作信息提示，agent 看一眼即可继续原方向
///   - Warning：建议 agent 调整下一步动作
///   - Blocking：必须 LLM 处理（注入后 agent **不能**立即 task_complete，
///     至少需要再跑一个 step 处理这条消息——由 engine 主循环兑现）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HookSeverity {
    Info,
    Warning,
    Blocking,
}

impl HookSeverity {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Blocking => "blocking",
        }
    }
}

/// Hook 执行结果。决定下游主循环做什么。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum HookOutcome {
    /// 通过，agent 继续正常流程
    Pass,
    /// 注入一条 user message 进 conversation。
    /// engine 主循环把 content 当作 user message push 进 messages，
    /// 下一次 LLM 调用就能看到。
    InjectMessage {
        content: String,
        severity: HookSeverity,
    },
    /// 强制终止。
    ///
    /// - `terminal: true` → 整 agent 立即 failed（用户没救）
    /// - `terminal: false` → 当前 step 结束（不继续后续 hook），agent 仍可下 step
    PreventContinuation { reason: String, terminal: bool },
}

/// 给 hook 执行时的所有上下文。
///
/// 字段为 owned 而非 `&references`——见模块文档"设计取舍"。
///
/// 字段长度全部 cap：避免 hook 上下文把 process memory 撑爆，也避免长
/// `last_assistant_text` 进配置 hook 命令的 stdin 时被 64KB pipe 截断。
#[derive(Debug, Clone, Serialize)]
pub struct HookContext {
    pub agent_id: String,
    pub mission_id: String,
    pub workspace_path: String,
    pub step: u32,
    pub phase: HookPhase,
    /// 最近 N 个 message 的摘要（实际 message 数 + 最后 assistant 文本 + 最近工具名）
    pub messages_summary: HookMessagesSummary,
    /// PostToolUse phase 时填，其它 phase 通常为 None
    pub last_tool_use: Option<HookToolUseInfo>,
    /// Stop / TaskCompleted phase 时填 task_complete 调用的 summary
    pub task_complete_summary: Option<String>,
}

/// messages 状态摘要。
#[derive(Debug, Clone, Serialize)]
pub struct HookMessagesSummary {
    pub total_count: usize,
    /// 截前 1KB 给 hook 看，避免大文本进 hook stdin
    pub last_assistant_text: Option<String>,
    /// 最近 5 个工具名（按时间倒序）
    pub recent_tool_uses: Vec<String>,
}

/// PostToolUse 时携带的具体工具信息。
#[derive(Debug, Clone, Serialize)]
pub struct HookToolUseInfo {
    pub tool_use_id: String,
    pub tool_name: String,
    pub input: serde_json::Value,
    /// 截前 2KB；完整 output 已在 events 表
    pub output_excerpt: String,
    pub is_error: bool,
}

/// Hook 通用 trait。所有 hook 类型（builtin / command / 未来 LLM-eval）实现此 trait。
///
/// **`async fn` 用 `async_trait`** 因 trait method 还不能直接 async（Rust 1.85
/// stable）。Send + Sync 必需——registry 持有 Arc 跨任务共享。
#[async_trait::async_trait]
pub trait AgentHook: Send + Sync {
    /// hook 标识符。前端 timeline + 日志显示用。建议格式：`"namespace/name"`，
    /// 如 `"builtin/guardrail"` / `"workspace/lint-after-write"`。
    fn name(&self) -> &str;

    /// 此 hook 关心哪些 phase。registry 按 phase 过滤，不在列表的 phase 直接 skip。
    fn phases(&self) -> &[HookPhase];

    /// 进一步过滤：matcher 返回 false 时该 phase 也 skip。
    /// 默认全 match；PostToolUse hook 可根据 ctx.last_tool_use.tool_name 选择性触发。
    #[allow(unused_variables)]
    fn matches(&self, ctx: &HookContext) -> bool {
        true
    }

    /// 主执行函数。返回 [`HookOutcome`] 决定主循环动作。
    ///
    /// 实现注意：
    /// - **不要 panic**：hook panic 会让主循环 abort agent，比错误返回 InjectMessage 灾难得多
    /// - **respect cancel**：长耗时 hook 应检查 `CancellationToken`（未来 ctx 可能加），
    ///   现阶段靠 timeout 兜底
    async fn execute(&self, ctx: &HookContext) -> HookOutcome;
}

/// 一条收集到的 InjectMessage 结果（含来源 hook 名，方便 timeline 渲染）。
#[derive(Debug, Clone, Serialize)]
pub struct HookInjection {
    pub hook_name: String,
    pub content: String,
    pub severity: HookSeverity,
}

/// 一个 phase 的所有 hook 执行后的整体结果。
#[derive(Debug, Clone, Serialize)]
pub enum PhaseResult {
    /// 所有 hook 都 Pass
    Pass,
    /// 至少一个 hook InjectMessage，未触发 Prevent
    Injected(Vec<HookInjection>),
    /// 某个 hook 触发 Prevent，**后续 hook 不再执行**
    Prevented {
        hook_name: String,
        reason: String,
        terminal: bool,
    },
}

/// Hook 注册表 —— 按 phase 路由 hook，统一执行语义。
///
/// 不是 `HashMap<Phase, Vec<Hook>>` 而是 `Vec<Hook>` + 每次 phase 触发时 filter，
/// 原因：
///   1. hook 总数 < 20，filter 开销可忽略
///   2. 单测里 mock hook 时不用绑定具体 phase
///   3. 一个 hook 关心多 phase 时只注册一次（HashMap 方案要每 phase 注册一份）
#[derive(Default)]
pub struct HookRegistry {
    hooks: Vec<Arc<dyn AgentHook>>,
}

impl HookRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册一个 hook。线程不安全：必须在 agent 启动前完成所有注册。
    pub fn register(&mut self, hook: Arc<dyn AgentHook>) {
        self.hooks.push(hook);
    }

    /// 合并另一个 registry（用于"built-in + user-global + workspace"分层加载）。
    pub fn merge(&mut self, other: HookRegistry) {
        self.hooks.extend(other.hooks);
    }

    /// 当前已注册的 hook 数。debug / metrics 用。
    pub fn hook_count(&self) -> usize {
        self.hooks.len()
    }

    /// 执行某 phase 的所有 matched hook。
    ///
    /// **执行顺序**：注册顺序（built-in 先，user-config 后）。
    /// **短路语义**：任意 hook 返回 PreventContinuation → 立即返回 Prevented，
    ///   后续 hook 不再执行（包括它们的 InjectMessage 也丢弃）。
    /// **聚合语义**：多个 hook 都 InjectMessage → 全部收集到 Injected(vec)，
    ///   caller 按顺序 push 进 messages。
    pub async fn execute_phase(&self, ctx: &HookContext) -> PhaseResult {
        let mut injections = Vec::new();
        for hook in &self.hooks {
            if !hook.phases().contains(&ctx.phase) {
                continue;
            }
            if !hook.matches(ctx) {
                continue;
            }
            let outcome = hook.execute(ctx).await;
            match outcome {
                HookOutcome::Pass => {}
                HookOutcome::InjectMessage { content, severity } => {
                    injections.push(HookInjection {
                        hook_name: hook.name().to_string(),
                        content,
                        severity,
                    });
                }
                HookOutcome::PreventContinuation { reason, terminal } => {
                    return PhaseResult::Prevented {
                        hook_name: hook.name().to_string(),
                        reason,
                        terminal,
                    };
                }
            }
        }
        if injections.is_empty() {
            PhaseResult::Pass
        } else {
            PhaseResult::Injected(injections)
        }
    }
}

// ============================================================================
// 单测
// ============================================================================

#[cfg(test)]
mod tests {
    //! Phase A 不变量回归：
    //!   1. 空 registry → 任何 phase 都 Pass
    //!   2. 多个 InjectMessage 全部收集（顺序 = 注册顺序）
    //!   3. 第一个 Prevent 短路：后续 hook 不执行
    //!   4. phase 过滤：hook 只在声明的 phase 运行
    //!   5. matcher 过滤：matches=false 的 hook 跳过
    //!   6. merge：合并两个 registry 后顺序 = registry_a 后接 registry_b
    //!   7. phase as_str / from_str round-trip 稳定
    //!   8. severity / outcome JSON 序列化稳定（hook 命令协议的一部分）

    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn mock_ctx(phase: HookPhase) -> HookContext {
        HookContext {
            agent_id: "agent-1".into(),
            mission_id: "mission-1".into(),
            workspace_path: "/tmp/ws".into(),
            step: 5,
            phase,
            messages_summary: HookMessagesSummary {
                total_count: 10,
                last_assistant_text: Some("ok".into()),
                recent_tool_uses: vec!["read_file".into()],
            },
            last_tool_use: None,
            task_complete_summary: None,
        }
    }

    /// 可配置 mock hook：phase 列表 + outcome
    struct MockHook {
        name: String,
        phases: Vec<HookPhase>,
        outcome_fn: Arc<dyn Fn() -> HookOutcome + Send + Sync>,
        call_count: AtomicUsize,
        match_fn: Arc<dyn Fn(&HookContext) -> bool + Send + Sync>,
    }

    impl MockHook {
        fn pass_all(name: &str, phases: Vec<HookPhase>) -> Arc<Self> {
            Arc::new(Self {
                name: name.into(),
                phases,
                outcome_fn: Arc::new(|| HookOutcome::Pass),
                call_count: AtomicUsize::new(0),
                match_fn: Arc::new(|_| true),
            })
        }

        fn injecting(name: &str, phases: Vec<HookPhase>, content: &str) -> Arc<Self> {
            let content = content.to_string();
            Arc::new(Self {
                name: name.into(),
                phases,
                outcome_fn: Arc::new(move || HookOutcome::InjectMessage {
                    content: content.clone(),
                    severity: HookSeverity::Warning,
                }),
                call_count: AtomicUsize::new(0),
                match_fn: Arc::new(|_| true),
            })
        }

        fn preventing(name: &str, phases: Vec<HookPhase>, terminal: bool) -> Arc<Self> {
            Arc::new(Self {
                name: name.into(),
                phases,
                outcome_fn: Arc::new(move || HookOutcome::PreventContinuation {
                    reason: "test-prevent".into(),
                    terminal,
                }),
                call_count: AtomicUsize::new(0),
                match_fn: Arc::new(|_| true),
            })
        }

        fn calls(&self) -> usize {
            self.call_count.load(Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl AgentHook for MockHook {
        fn name(&self) -> &str {
            &self.name
        }
        fn phases(&self) -> &[HookPhase] {
            &self.phases
        }
        fn matches(&self, ctx: &HookContext) -> bool {
            (self.match_fn)(ctx)
        }
        async fn execute(&self, _ctx: &HookContext) -> HookOutcome {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            (self.outcome_fn)()
        }
    }

    #[tokio::test]
    async fn empty_registry_passes_any_phase() {
        let reg = HookRegistry::new();
        for phase in [
            HookPhase::PreLlmCall,
            HookPhase::PostToolUse,
            HookPhase::Stop,
            HookPhase::TaskCompleted,
        ] {
            assert!(matches!(
                reg.execute_phase(&mock_ctx(phase)).await,
                PhaseResult::Pass
            ));
        }
    }

    #[tokio::test]
    async fn collects_multiple_injections_in_register_order() {
        let mut reg = HookRegistry::new();
        reg.register(MockHook::injecting("h1", vec![HookPhase::Stop], "first"));
        reg.register(MockHook::injecting("h2", vec![HookPhase::Stop], "second"));
        reg.register(MockHook::injecting("h3", vec![HookPhase::Stop], "third"));

        match reg.execute_phase(&mock_ctx(HookPhase::Stop)).await {
            PhaseResult::Injected(injs) => {
                assert_eq!(injs.len(), 3);
                assert_eq!(injs[0].hook_name, "h1");
                assert_eq!(injs[0].content, "first");
                assert_eq!(injs[1].content, "second");
                assert_eq!(injs[2].content, "third");
            }
            other => panic!("expected Injected, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn prevent_short_circuits_subsequent_hooks() {
        let mut reg = HookRegistry::new();
        let h1 = MockHook::injecting("h1", vec![HookPhase::Stop], "before");
        let h2 = MockHook::preventing("h2", vec![HookPhase::Stop], true);
        let h3 = MockHook::injecting("h3", vec![HookPhase::Stop], "should-not-run");
        reg.register(h1.clone());
        reg.register(h2.clone());
        reg.register(h3.clone());

        match reg.execute_phase(&mock_ctx(HookPhase::Stop)).await {
            PhaseResult::Prevented {
                hook_name,
                reason,
                terminal,
            } => {
                assert_eq!(hook_name, "h2");
                assert_eq!(reason, "test-prevent");
                assert!(terminal);
            }
            other => panic!("expected Prevented, got {other:?}"),
        }
        // 关键不变量：h3 没被执行（短路语义）
        assert_eq!(h3.calls(), 0);
        // h1 跑了（在 prevent 之前）但其 InjectMessage 被丢弃（Prevented 优先）
        assert_eq!(h1.calls(), 1);
    }

    #[tokio::test]
    async fn hook_only_runs_in_declared_phase() {
        let mut reg = HookRegistry::new();
        // 只关心 PreLlmCall 的 hook
        let h = MockHook::injecting("pre-only", vec![HookPhase::PreLlmCall], "x");
        reg.register(h.clone());

        // Stop phase → 不应触发
        let r = reg.execute_phase(&mock_ctx(HookPhase::Stop)).await;
        assert!(matches!(r, PhaseResult::Pass));
        assert_eq!(h.calls(), 0);

        // PreLlmCall phase → 触发
        let r = reg.execute_phase(&mock_ctx(HookPhase::PreLlmCall)).await;
        assert!(matches!(r, PhaseResult::Injected(_)));
        assert_eq!(h.calls(), 1);
    }

    #[tokio::test]
    async fn hook_with_multiple_phases_runs_in_each() {
        let mut reg = HookRegistry::new();
        let h = MockHook::pass_all("multi", vec![HookPhase::PreLlmCall, HookPhase::Stop]);
        reg.register(h.clone());

        reg.execute_phase(&mock_ctx(HookPhase::PreLlmCall)).await;
        reg.execute_phase(&mock_ctx(HookPhase::Stop)).await;
        reg.execute_phase(&mock_ctx(HookPhase::PostToolUse)).await; // 没声明，不跑

        assert_eq!(h.calls(), 2);
    }

    #[tokio::test]
    async fn matcher_filter_skips_hook() {
        let mut reg = HookRegistry::new();
        // 用 builder 模式注入 matcher
        let h = Arc::new(MockHook {
            name: "match-only-step-5".into(),
            phases: vec![HookPhase::PostToolUse],
            outcome_fn: Arc::new(|| HookOutcome::InjectMessage {
                content: "x".into(),
                severity: HookSeverity::Info,
            }),
            call_count: AtomicUsize::new(0),
            match_fn: Arc::new(|ctx| ctx.step == 5),
        });
        reg.register(h.clone());

        // step=5 → 触发
        let mut ctx = mock_ctx(HookPhase::PostToolUse);
        ctx.step = 5;
        let r = reg.execute_phase(&ctx).await;
        assert!(matches!(r, PhaseResult::Injected(_)));
        assert_eq!(h.calls(), 1);

        // step=6 → matcher 否决
        ctx.step = 6;
        let r = reg.execute_phase(&ctx).await;
        assert!(matches!(r, PhaseResult::Pass));
        assert_eq!(h.calls(), 1, "matcher 否决后不应 execute");
    }

    #[tokio::test]
    async fn merge_preserves_order() {
        let mut a = HookRegistry::new();
        a.register(MockHook::injecting("a1", vec![HookPhase::Stop], "a-one"));
        a.register(MockHook::injecting("a2", vec![HookPhase::Stop], "a-two"));

        let mut b = HookRegistry::new();
        b.register(MockHook::injecting("b1", vec![HookPhase::Stop], "b-one"));

        a.merge(b);
        assert_eq!(a.hook_count(), 3);

        match a.execute_phase(&mock_ctx(HookPhase::Stop)).await {
            PhaseResult::Injected(injs) => {
                assert_eq!(injs[0].hook_name, "a1");
                assert_eq!(injs[1].hook_name, "a2");
                assert_eq!(injs[2].hook_name, "b1");
            }
            _ => panic!(),
        }
    }

    #[test]
    fn phase_round_trip_str() {
        for p in [
            HookPhase::PreLlmCall,
            HookPhase::PostSampling,
            HookPhase::PostToolUse,
            HookPhase::PreCompact,
            HookPhase::PostCompact,
            HookPhase::Stop,
            HookPhase::TaskCompleted,
        ] {
            assert_eq!(HookPhase::from_str(p.as_str()), Some(p));
        }
        assert_eq!(HookPhase::from_str("BogusPhase"), None);
    }

    #[test]
    fn severity_labels_stable() {
        // hook 命令协议依赖这些字串，改了等于破坏外部脚本
        assert_eq!(HookSeverity::Info.as_str(), "info");
        assert_eq!(HookSeverity::Warning.as_str(), "warning");
        assert_eq!(HookSeverity::Blocking.as_str(), "blocking");
    }

    #[test]
    fn hook_outcome_serializes_to_stable_json() {
        // hook command 解析 stdout 时按这些 JSON 字段名匹配，改名 = 破坏协议
        let p = HookOutcome::Pass;
        let json = serde_json::to_value(&p).unwrap();
        assert_eq!(json, serde_json::json!("Pass"));

        let inj = HookOutcome::InjectMessage {
            content: "hi".into(),
            severity: HookSeverity::Warning,
        };
        let json = serde_json::to_value(&inj).unwrap();
        assert_eq!(json["InjectMessage"]["content"], "hi");
        assert_eq!(json["InjectMessage"]["severity"], "Warning");

        let prev = HookOutcome::PreventContinuation {
            reason: "no".into(),
            terminal: true,
        };
        let json = serde_json::to_value(&prev).unwrap();
        assert_eq!(json["PreventContinuation"]["reason"], "no");
        assert_eq!(json["PreventContinuation"]["terminal"], true);
    }

    #[tokio::test]
    async fn prevent_non_terminal_carries_flag() {
        let mut reg = HookRegistry::new();
        reg.register(MockHook::preventing("h", vec![HookPhase::Stop], false));
        match reg.execute_phase(&mock_ctx(HookPhase::Stop)).await {
            PhaseResult::Prevented { terminal, .. } => assert!(!terminal),
            _ => panic!(),
        }
    }

    #[tokio::test]
    async fn hooks_in_other_phases_not_counted() {
        // 守住 phase 过滤的不变量：注册了一个只跑 PostToolUse 的 hook，
        // 在 PreLlmCall 触发时不能误调用
        let mut reg = HookRegistry::new();
        let h = MockHook::pass_all("post-tool", vec![HookPhase::PostToolUse]);
        reg.register(h.clone());

        for phase in [
            HookPhase::PreLlmCall,
            HookPhase::PostSampling,
            HookPhase::PreCompact,
            HookPhase::PostCompact,
            HookPhase::Stop,
            HookPhase::TaskCompleted,
        ] {
            reg.execute_phase(&mock_ctx(phase)).await;
        }
        assert_eq!(h.calls(), 0, "hook 不应在未声明 phase 被调用");
    }

    // ============================================================================
    // E2E-style: compound multi-hook scenarios
    // ============================================================================
    //
    // 这些测试模拟现实场景：用户在 `.miragenty/hooks.json` 注册多个 hook（例如
    // "tsc → eslint → npm test"），它们的组合行为必须可预测。

    /// 场景：3 个 hook 在同 phase，前两个 Inject、第三个 Prevent → 第三个的 Prevent
    /// 优先（短路），前两个的 Inject 全部丢弃。
    ///
    /// 这是用户 deploy 一个 "vetoing safety check" 时的核心需求：safety check 拦下，
    /// 不应让前面的非阻塞 inject 还塞进 conversation 让 agent 误以为可以继续。
    #[tokio::test]
    async fn inject_then_prevent_discards_pending_injects() {
        let mut reg = HookRegistry::new();
        reg.register(MockHook::injecting(
            "tsc",
            vec![HookPhase::Stop],
            "tsc passed",
        ));
        reg.register(MockHook::injecting(
            "lint",
            vec![HookPhase::Stop],
            "lint passed",
        ));
        // 关键：safety check 在最后注册，前两个都跑了，但它 prevent 后整体结果是 Prevented
        reg.register(MockHook::preventing(
            "safety-check",
            vec![HookPhase::Stop],
            true,
        ));

        match reg.execute_phase(&mock_ctx(HookPhase::Stop)).await {
            PhaseResult::Prevented { hook_name, .. } => assert_eq!(hook_name, "safety-check"),
            other => panic!("expected Prevented, got {other:?}"),
        }
    }

    /// 场景：1 个 hook 同时关心 PostToolUse + Stop。每次对应 phase 触发都 Inject，
    /// 验证 phases() 数组形式的 hook 没有重复执行也没有遗漏。
    #[tokio::test]
    async fn hook_with_two_phases_inject_each() {
        let mut reg = HookRegistry::new();
        let h = MockHook::injecting(
            "lint",
            vec![HookPhase::PostToolUse, HookPhase::Stop],
            "lint output",
        );
        reg.register(h.clone());

        // PostToolUse → 1 次
        match reg.execute_phase(&mock_ctx(HookPhase::PostToolUse)).await {
            PhaseResult::Injected(injs) => assert_eq!(injs.len(), 1),
            _ => panic!(),
        }
        // Stop → 1 次
        match reg.execute_phase(&mock_ctx(HookPhase::Stop)).await {
            PhaseResult::Injected(injs) => assert_eq!(injs.len(), 1),
            _ => panic!(),
        }
        // 总共 2 次（不是 1 次也不是 4 次）
        assert_eq!(h.calls(), 2);
    }

    /// 场景：HookOutcome JSON serde round-trip 稳定——这是 Phase C 引入 Command hook 后
    /// hook 命令通过 stdout 传 Outcome 的协议契约。任何字段名变更都会导致用户的 hook
    /// 脚本失效。**改一行就 break 兼容性的回归测试**。
    #[test]
    fn outcome_roundtrip_via_json_preserves_data() {
        let cases = vec![
            HookOutcome::Pass,
            HookOutcome::InjectMessage {
                content: "hello\nworld".into(),
                severity: HookSeverity::Blocking,
            },
            HookOutcome::PreventContinuation {
                reason: "exit non-zero".into(),
                terminal: false,
            },
        ];
        for original in cases {
            let json = serde_json::to_string(&original).unwrap();
            let decoded: HookOutcome = serde_json::from_str(&json).unwrap();
            // 直接比对枚举不够（HookOutcome 没派生 PartialEq），用 JSON 二次序列化对比
            let json2 = serde_json::to_string(&decoded).unwrap();
            assert_eq!(json, json2, "roundtrip lossy: {json}");
        }
    }

    /// 场景：HookPhase JSON serde 稳定——配置文件 `.miragenty/hooks.json` 里 phase 用
    /// 字符串（"Stop"、"PostToolUse" 等），Phase C CommandHook 加载时按 from_str 解析。
    /// 改名 = 用户配置失效。
    #[test]
    fn phase_serde_string_stable() {
        for p in [
            HookPhase::PreLlmCall,
            HookPhase::PostSampling,
            HookPhase::PostToolUse,
            HookPhase::PreCompact,
            HookPhase::PostCompact,
            HookPhase::Stop,
            HookPhase::TaskCompleted,
        ] {
            let json = serde_json::to_value(&p).unwrap();
            // serde 默认枚举 unit variant 序列化为字符串
            let s = json.as_str().expect("phase 必须序列化为字符串");
            assert_eq!(HookPhase::from_str(s), Some(p));
        }
    }
}
