# P2-1：通用 Stop Hook 体系 —— 每 step 边界开放可编程钩子

> **目标**：把现有"仅在 `task_complete` 时跑 guardrails"扩展为**全 phase 可注入 hook 体系**：PostSampling / PostToolUse / PreCompact / PostCompact / Stop / TaskCompleted。每个 hook 可以 Pass / InjectMessage（让 agent 继续做点别的）/ PreventContinuation（强制终止）。
>
> **对标**：Claude Code `utils/hooks.ts` + `query/stopHooks.ts` + 用户的 `.claude/hooks.json` 配置文件协议。
> **本系列定位**：单 agent 鲁棒性升级 7 篇之 7（架构级）。前置依赖：理想情况 P0-3（recovery_log）。

---

## 实施状态（2026-05-20 更新）

P2-1 是 7 篇里**最重**的一条（~1370 行 + 5 个 commit + 横跨 trait/engine/CommandHook/UI），分三阶段落地：

### Phase A（已完成 ✅）—— Trait + Registry + 单测

落地内容（`src-tauri/src/agent/hooks/mod.rs`）：
1. **`HookPhase`** enum（7 个 phase）+ `as_str` / `from_str` round-trip
2. **`HookOutcome`** enum：`Pass` / `InjectMessage { content, severity }` / `PreventContinuation { reason, terminal }`
3. **`HookSeverity`** enum：`Info` / `Warning` / `Blocking`
4. **`HookContext`** struct（owned 字段避免生命周期地狱）+ `HookMessagesSummary` + `HookToolUseInfo`
5. **`AgentHook`** trait（async + Send + Sync），含 `name` / `phases` / `matches` / `execute`
6. **`HookRegistry`**：注册顺序执行、phase 过滤、matcher 过滤、Prevent 短路、Inject 聚合
7. **12 个单测**：empty / multi-inject / prevent-short-circuit / phase-filter / matcher-filter / merge-order / JSON-stability / phase round-trip

测试结果：541 全过（+12 自 P1-2 Phase A 完成时的 529）。**0 行为变化**——`engine.rs` 还未接线，registry 永远空。

### Phase B（已完成 ✅）—— Engine 接线 + Compound 测试

落地内容：
1. **`engine.rs`** 加 `hook_registry: Arc<HookRegistry>` 字段 + `with_hooks()` builder
2. **5 处 phase 调用点**接线：PreLlmCall / PostSampling / PreCompact / PostCompact / PostToolUse / Stop / TaskCompleted（实际写了 7 处覆盖 6 个 phase，PreCompact + PostCompact 围绕 microcompact 联动）
3. **`dispatch_hook_phase` helper**：空 registry fast path（0 行为变化）；`Injected` 按注册顺序聚合成单条 user message 注入 + emit `hook_inject`；`Prevented` emit `hook_prevented` 后返回 `HookFatal::{Terminal,StepAborted}`
4. **`build_hook_context` helper**：1KB 截前 last_assistant_text + 5 项 recent_tool_uses + DB 反查 mission_id
5. **`HookFatal` enum**：Terminal → `mark_task_failed_with_reason` 后 `Ok(AgentStatus::Failed)`；StepAborted → 跳本 step 继续（TaskCompleted 上 StepAborted 视作 fail 避免 completed-but-not-finalized）
6. **Migration 028**：`agent_events.kind` CHECK 加 `hook_executed` / `hook_inject` / `hook_prevented`；前端 `AgentEventKind` 同步
7. **Compound integration tests**（hooks/mod.rs）：inject-then-prevent 短路语义 + multi-phase hook 行为 + Outcome JSON roundtrip + Phase serde 稳定性（4 个新增）

测试结果：541 → 562 全过。默认空 registry 时 byte-identical 行为。

GuardrailHook 包装暂未做：现有 task_complete → guardrail 路径不动（行为对等），未来用户需要"Stop hook 跑 npm test"时再做包装迁移。

### Phase C（已完成 ✅）—— Command Hook + Config + UI

落地内容：
1. **`agent/hooks/command.rs`**：`CommandHook` impl AgentHook —— `sh -c <command>` + 60s timeout + stdin/stdout JSON 协议 + matcher 子串匹配。10 个单测覆盖 Pass / Inject / Prevent stdout / 非 0 exit 转 advisory inject / timeout / malformed JSON 退化为 Pass
2. **`agent/hooks/config.rs`**：`load_workspace_hooks` 从 `workspace/.miragenty/hooks.json` 加载——`allow_command_hooks=false` 立即 short-circuit 不读文件；schema 校验（未知 phase / 空 command 整条丢）+ 7 个单测
3. **`AppConfig.allow_command_hooks: bool`** 默认 false + ConfigResponse / UpdateConfigRequest 字段 + `update_config` 写入时 `tracing::info!` 记录开关变更（audit log）
4. **`scheduler.rs`** 在 spawn engine 前调用 loader，仅在 `hook_count > 0` 时 `with_hooks`
5. **`SettingsView.tsx` Developer 区块**：allow_command_hooks segmented control + i18n（en/zh）含 RCE 警告说明 + 即点即保存（无 dirty 二段式，避免幻觉成功）
6. **前端 IPC 类型** `ConfigResponse.allow_command_hooks` / `UpdateConfigRequest.allow_command_hooks` 同步

测试结果：562 全过（17 个新增：10 command + 7 config）。

**安全模型**（必读，见 `hooks/config.rs` 模块文档）：
- 默认禁用 → 完全等同 Phase B 行为
- 仅读 workspace 内 hooks.json（不接受 user-global 路径，限制爆炸半径到单 repo）
- JSON 解析失败 / 命令非 0 退出 → 退化为 Pass / advisory inject，**不让一个 hook 错误让 agent fail**
- 60s timeout 兜底防恶意阻塞

未做（留作未来 PR）：Workspace hooks review modal（mission 启动前列出已加载 hook 让用户二次确认）。当前依赖 Settings 的全局 toggle + workspace 范围限制 + 用户对自己 repo 内容的审计。

---

---

## 1. 现状

### 1.1 现有"hook 雏形"

| 现有机制 | 位置 | 触发点 | 限制 |
| --- | --- | --- | --- |
| Guardrail | `agent/guardrail.rs` | 仅 `task_complete` 调用时 | 单一 hook 类型；feedback 路径硬编码 |
| Approval Gate | `agent/approval_gate.rs` | unsafe tool 调用前 | UX 走 approval queue UI，非脚本化 |
| Agent Notes (FM-06) | `commands/agent.rs:inject_mission_note` | 用户主动 inject | 单向"用户→agent"，非"系统→agent" |
| Cancel Token | tokio_util CancellationToken | 任意时刻 | 二值（cancel/not），无 inject 语义 |

**共同缺陷**：没有"任意 step 边界、任意 condition 触发、任意 action 输出"的统一抽象。用户没法说"每次 tool_use 后跑一次 tsc，编译错就让 agent 自己修"——必须改 engine.rs 或写新 guardrail。

### 1.2 Claude Code 的 Stop Hook 体系

```typescript
// query/stopHooks.ts
const generator = executeStopHooks(
    permissionMode,
    toolUseContext.abortController.signal,
    undefined,
    stopHookActive ?? false,
    toolUseContext.agentId,
    toolUseContext,
    [...messagesForQuery, ...assistantMessages],
    toolUseContext.agentType,
)

for await (const result of generator) {
    if (result.message) yield result.message              // hook 自己 emit 用户可见消息
    if (result.blockingError) blockingErrors.push(...)    // hook 阻塞此次 stop（注入回 conversation）
    if (result.preventContinuation) preventedContinuation = true   // hook 强制 stop
}
```

用户的 `~/.claude/hooks.json` 长这样：

```json
{
  "PostToolUse": [
    { "matcher": "Write|Edit", "hooks": [{ "type": "command", "command": "npx tsc --noEmit" }] }
  ],
  "Stop": [
    { "hooks": [{ "type": "command", "command": "npm test" }] }
  ]
}
```

机制本质：**hook 是一个可执行命令（或内置 hook fn），engine 在 phase 边界把上下文（messages / 工具产物 / agent meta）作为 stdin/env 喂给它，stdout/exit-code 决定下一步**。

---

## 2. 目标行为

### 2.1 Hook Phase 清单（Miragenty 适配版）

| Phase | 触发时机 | 典型用途 |
| --- | --- | --- |
| `PreLlmCall` | 每 step 发起 LLM 调用之前 | 注入额外 system context；预算检查 |
| `PostSampling` | LLM 响应到手、tool_use 解析前 | 记录响应；reasoning 内容分析 |
| `PostToolUse` | 每次 tool_use 执行后 | tsc/lint/test 检查（最高频用例） |
| `PreCompact` | microcompact / reactive compact 触发前 | 持久化要保留的关键上下文 |
| `PostCompact` | compact 完成后 | 重新注入丢失的 context |
| `Stop` | LLM 不再产 tool_use（自然 turn 结束）或调 task_complete | 全套质量检查、提交前钩子 |
| `TaskCompleted` | guardrail 全部通过、status → completed 之前 | publish artifact、send notification |

**Miragenty 特化**：不引入 Claude Code 的 `TeammateIdle`（multi-agent 概念已在 Scheduler 层）。`Stop` 在 Miragenty 等同于 `task_complete + guardrail_pass`。

### 2.2 Hook 返回类型

```rust
pub enum HookOutcome {
    /// 通过，agent 继续正常流程
    Pass,
    /// 注入一条 user message 进 conversation，让 agent 看到 hook 输出后再决定
    InjectMessage { content: String, severity: HookSeverity },
    /// 强制终止当前 step / 整 agent
    PreventContinuation { reason: String, terminal: bool },
}

pub enum HookSeverity {
    Info,        // 给 LLM 一条提示，期望 LLM 不改方向
    Warning,     // 给 LLM 一条警告，期望 LLM 调整下一步
    Blocking,    // 必须 LLM 处理（注入后强制要求至少 1 step 后才能 task_complete）
}
```

### 2.3 Hook 实现源

```rust
pub enum HookKind {
    /// 内置 hook：trait object，Miragenty 自带（如现有 guardrail 包装）
    Builtin(Arc<dyn AgentHook>),
    /// 外部命令：shell command，stdin 喂 JSON 上下文，stdout 解析 JSON HookOutcome
    Command { cmd: String, timeout_secs: u32 },
    /// 内置内置 LLM 微调（如 "调用小模型问问看代码改得对吗"）
    LlmEvaluator { model: String, prompt_template: String },
}
```

第一版**仅实现 Builtin + Command**；LlmEvaluator 后续 PR。

### 2.4 Hook 配置位置

| 配置层 | 文件 / 表 | 适用范围 |
| --- | --- | --- |
| Workspace（最常用） | `<workspace>/.miragenty/hooks.json` | 当前 mission 所在 git repo 全部 mission |
| User-global | `~/Library/Application Support/com.miragenty.app/hooks.json` | 用户所有 mission |
| Mission-level | DB `missions.hooks_override JSON` | 单个 mission 临时覆盖 |
| Built-in | 编译期注册 | Miragenty 内置（如 Guardrail wrapped 版） |

加载顺序：built-in → user-global → workspace → mission-level，后者覆盖前者同 phase 同 matcher 配置。

### 2.5 安全约束（不要重蹈 RCE 覆辙）

- **Command 类 hook 默认禁用**，需用户在 Settings 显式开启
- 启用后，命令执行 wrapped in `tokio::process::Command` + 60s timeout + 用户当前 PATH（不继承 sudo / SSH 环境）
- workspace 级 hooks.json 加载前提示用户审阅（首次见到 + hash 变更时弹 approval）
- 提供 `dry_run` 模式：hooks 只输出"将要运行"日志，不真执行

---

## 3. 设计细节

### 3.1 AgentHook trait

```rust
//! 通用 hook trait。所有 hook 类型（Builtin / Command / LlmEvaluator）实现此 trait。
//!
//! 上下文用 owned struct 而非 &references，避免 hook future 借用 engine 的麻烦。

use serde::Serialize;
use std::sync::Arc;

#[derive(Debug, Clone, Serialize)]
pub struct HookContext {
    pub agent_id: String,
    pub mission_id: String,
    pub workspace_path: String,
    pub step: u32,
    pub phase: HookPhase,
    pub messages_summary: HookMessagesSummary,
    pub last_tool_use: Option<HookToolUseInfo>,
    pub task_complete_summary: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub enum HookPhase {
    PreLlmCall,
    PostSampling,
    PostToolUse,
    PreCompact,
    PostCompact,
    Stop,
    TaskCompleted,
}

#[derive(Debug, Clone, Serialize)]
pub struct HookMessagesSummary {
    pub total_count: usize,
    pub last_assistant_text: Option<String>,    // 截前 1KB
    pub recent_tool_uses: Vec<String>,           // 最近 5 个工具名
}

#[derive(Debug, Clone, Serialize)]
pub struct HookToolUseInfo {
    pub tool_use_id: String,
    pub tool_name: String,
    pub input: serde_json::Value,
    pub output_excerpt: String,                  // 截前 2KB
    pub is_error: bool,
}

#[async_trait::async_trait]
pub trait AgentHook: Send + Sync {
    /// hook 名字（前端/日志展示）
    fn name(&self) -> &str;
    
    /// 此 hook 关心哪些 phase
    fn phases(&self) -> &[HookPhase];
    
    /// matcher：仅 PostToolUse 时关心 tool name；其它 phase 无视
    fn matches(&self, ctx: &HookContext) -> bool { true }
    
    /// 主执行函数
    async fn execute(&self, ctx: &HookContext) -> HookOutcome;
}
```

### 3.2 HookRegistry

```rust
pub struct HookRegistry {
    hooks: Vec<Arc<dyn AgentHook>>,
}

impl HookRegistry {
    pub fn new() -> Self { Self { hooks: Vec::new() } }
    
    pub fn register(&mut self, hook: Arc<dyn AgentHook>) {
        self.hooks.push(hook);
    }
    
    /// 跑某 phase 的所有 matched hook（顺序：注册顺序）。
    /// 任意 hook 返回 PreventContinuation → 立即停（不继续后续 hook）
    /// 任意 hook 返回 InjectMessage → 收集到列表，全部 hook 跑完后一次性返回
    pub async fn execute_phase(&self, ctx: &HookContext) -> PhaseResult {
        let mut injections = Vec::new();
        for hook in &self.hooks {
            if !hook.phases().contains(&ctx.phase) { continue; }
            if !hook.matches(ctx) { continue; }
            let outcome = hook.execute(ctx).await;
            match outcome {
                HookOutcome::Pass => {}
                HookOutcome::InjectMessage { content, severity } => {
                    injections.push(HookInjection {
                        hook_name: hook.name().to_string(),
                        content, severity,
                    });
                }
                HookOutcome::PreventContinuation { reason, terminal } => {
                    return PhaseResult::Prevented { hook_name: hook.name().to_string(), reason, terminal };
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

pub enum PhaseResult {
    Pass,
    Injected(Vec<HookInjection>),
    Prevented { hook_name: String, reason: String, terminal: bool },
}

pub struct HookInjection {
    pub hook_name: String,
    pub content: String,
    pub severity: HookSeverity,
}
```

### 3.3 主循环集成点

每 phase 在 engine.rs 主循环对应位置插入：

```rust
// PreLlmCall：现 engine.rs:656 describe_llm_call 之前
let ctx = build_hook_context(HookPhase::PreLlmCall, ...);
match self.hook_registry.execute_phase(&ctx).await {
    PhaseResult::Pass => {}
    PhaseResult::Injected(injections) => {
        for inj in injections {
            messages.push(/* inj.content as user msg */);
            self.emit_event_with_meta(agent_id, step, "hook_inject", &inj.content, ...);
        }
    }
    PhaseResult::Prevented { reason, terminal: true, .. } => {
        // 强制终止整 agent
        self.update_agent_status(agent_id, "failed");
        return Ok(AgentStatus::Failed);
    }
    PhaseResult::Prevented { reason, terminal: false, .. } => {
        // 强制结束本 step（注入 reason 后下次 loop 跳过）
        // ...
    }
}
```

类似地：
- **PostSampling**：拿到 LlmResponse 之后、tool_use 解析之前
- **PostToolUse**：每个 tool_use 执行完之后（注意：streaming executor 下要在 drain_in_order 完成后 per-tool 触发）
- **PreCompact / PostCompact**：microcompact / reactive_compact 前后
- **Stop**：task_complete 调用 + guardrail 通过 之后；如 hook InjectMessage 则 agent **不** completed，注入回 conversation 继续 loop
- **TaskCompleted**：所有 Stop hook 通过、agent 即将 emit status_change=completed 之前

### 3.4 Guardrail 改造为内置 Stop Hook

把 `agent/guardrail.rs` 包装成一个内置 hook：

```rust
pub struct GuardrailHook {
    guardrails: Vec<Guardrail>,
}

#[async_trait::async_trait]
impl AgentHook for GuardrailHook {
    fn name(&self) -> &str { "builtin/guardrail" }
    fn phases(&self) -> &[HookPhase] { &[HookPhase::Stop] }
    
    async fn execute(&self, ctx: &HookContext) -> HookOutcome {
        let summary = ctx.task_complete_summary.as_deref().unwrap_or("");
        match run_guardrails(&self.guardrails, /* workspace */ ctx.workspace_path.as_ref(), summary).await {
            GuardrailOutcome::AllPassed => HookOutcome::Pass,
            GuardrailOutcome::Failed { feedback } => HookOutcome::InjectMessage {
                content: feedback,
                severity: HookSeverity::Blocking,
            },
        }
    }
}
```

`AgentEngine::new` 把这个 hook 加进 registry，等价于现有 guardrail 行为，**0 行为变化**。

### 3.5 Command Hook 实现

```rust
pub struct CommandHook {
    name: String,
    phases: Vec<HookPhase>,
    matcher: Option<regex::Regex>,    // 仅 PostToolUse 用
    command: String,
    timeout_secs: u64,
}

#[async_trait::async_trait]
impl AgentHook for CommandHook {
    fn name(&self) -> &str { &self.name }
    fn phases(&self) -> &[HookPhase] { &self.phases }
    
    fn matches(&self, ctx: &HookContext) -> bool {
        if let (Some(re), Some(tu)) = (&self.matcher, &ctx.last_tool_use) {
            re.is_match(&tu.tool_name)
        } else {
            true
        }
    }
    
    async fn execute(&self, ctx: &HookContext) -> HookOutcome {
        let stdin_json = serde_json::to_string(ctx).unwrap_or_default();
        let res = tokio::time::timeout(
            std::time::Duration::from_secs(self.timeout_secs),
            tokio::process::Command::new("sh")
                .arg("-c").arg(&self.command)
                .current_dir(&ctx.workspace_path)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .and_then(|mut child| async move {
                    if let Some(stdin) = child.stdin.take() {
                        use tokio::io::AsyncWriteExt;
                        let mut stdin = stdin;
                        let _ = stdin.write_all(stdin_json.as_bytes()).await;
                    }
                    child.wait_with_output().await
                }),
        ).await;
        match res {
            Ok(Ok(output)) => {
                let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                // 解析 stdout：尝试 JSON，否则按 exit_code 决定
                if let Ok(parsed) = serde_json::from_str::<HookOutcome>(&stdout) {
                    parsed
                } else if output.status.success() {
                    HookOutcome::Pass
                } else {
                    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                    HookOutcome::InjectMessage {
                        content: format!("Hook `{}` failed (exit {}). Output:\n{}\n{}",
                            self.name, output.status.code().unwrap_or(-1), stdout, stderr),
                        severity: HookSeverity::Warning,
                    }
                }
            }
            Ok(Err(e)) => HookOutcome::InjectMessage {
                content: format!("Hook `{}` could not start: {e}", self.name),
                severity: HookSeverity::Info,
            },
            Err(_) => HookOutcome::InjectMessage {
                content: format!("Hook `{}` timed out after {}s", self.name, self.timeout_secs),
                severity: HookSeverity::Warning,
            },
        }
    }
}
```

### 3.6 Config 加载

`agent/hooks_config.rs`：

```rust
#[derive(Debug, Deserialize)]
pub struct HooksFile {
    /// phase 名 -> hooks 列表
    #[serde(flatten)]
    pub by_phase: HashMap<String, Vec<HookEntry>>,
}

#[derive(Debug, Deserialize)]
pub struct HookEntry {
    /// 仅 PostToolUse 用，工具名匹配（regex）
    #[serde(default)]
    pub matcher: Option<String>,
    pub hooks: Vec<HookSpec>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum HookSpec {
    #[serde(rename = "command")]
    Command { command: String, #[serde(default = "default_timeout")] timeout_secs: u64 },
    // 后续可扩展 #[serde(rename = "llm_evaluator")] ...
}

pub fn load_workspace_hooks(workspace_path: &Path) -> Result<HookRegistry> {
    let path = workspace_path.join(".miragenty/hooks.json");
    if !path.exists() { return Ok(HookRegistry::new()); }
    let text = std::fs::read_to_string(&path)?;
    let file: HooksFile = serde_json::from_str(&text)?;
    let mut registry = HookRegistry::new();
    for (phase_str, entries) in file.by_phase {
        let phase = parse_phase(&phase_str)?;
        for entry in entries {
            let matcher_re = entry.matcher.as_deref().map(regex::Regex::new).transpose()?;
            for spec in entry.hooks {
                match spec {
                    HookSpec::Command { command, timeout_secs } => {
                        let hook = Arc::new(CommandHook {
                            name: format!("workspace/{:?}", phase),
                            phases: vec![phase.clone()],
                            matcher: matcher_re.clone(),
                            command, timeout_secs,
                        });
                        registry.register(hook);
                    }
                }
            }
        }
    }
    Ok(registry)
}
```

加载顺序在 `AgentEngine::new` 里：

```rust
let mut registry = HookRegistry::new();
registry.register(GuardrailHook::new(opts.guardrails.clone()));
if app_config.allow_command_hooks {
    // 如果用户允许了命令 hook，加载 user-global / workspace 配置
    if let Ok(reg) = load_user_global_hooks() { registry.merge(reg); }
    if let Ok(reg) = load_workspace_hooks(&workspace_root) { registry.merge(reg); }
}
```

### 3.7 用户可见性

新事件 kind：`hook_executed`（meta：phase、hook_name、outcome、duration_ms）。前端在 timeline 渲染为浅蓝色 chip。

Settings 加：
- Toggle "Allow command hooks"（默认 false）
- 按钮 "Review workspace hooks.json"（弹出 JSON 内容 + diff 上次版本）

---

## 4. 验收

### 4.1 单元测试

```rust
#[tokio::test]
async fn registry_passes_through_when_no_hooks() {
    let reg = HookRegistry::new();
    let ctx = mock_ctx(HookPhase::PostToolUse);
    assert!(matches!(reg.execute_phase(&ctx).await, PhaseResult::Pass));
}

#[tokio::test]
async fn registry_collects_multiple_injections() {
    let mut reg = HookRegistry::new();
    reg.register(MockHook::injecting("a"));
    reg.register(MockHook::injecting("b"));
    let ctx = mock_ctx(HookPhase::Stop);
    if let PhaseResult::Injected(injs) = reg.execute_phase(&ctx).await {
        assert_eq!(injs.len(), 2);
    } else { panic!() }
}

#[tokio::test]
async fn registry_short_circuits_on_prevent() {
    let mut reg = HookRegistry::new();
    reg.register(MockHook::injecting("first"));
    reg.register(MockHook::preventing());
    reg.register(MockHook::injecting("never-reached"));
    let ctx = mock_ctx(HookPhase::Stop);
    if let PhaseResult::Prevented { .. } = reg.execute_phase(&ctx).await { /* pass */ } 
    else { panic!() }
}

#[tokio::test]
async fn command_hook_timeout_returns_warning() {
    let hook = CommandHook { name: "x".into(), phases: vec![HookPhase::PostToolUse], 
        matcher: None, command: "sleep 5".into(), timeout_secs: 1 };
    let result = hook.execute(&mock_ctx(HookPhase::PostToolUse)).await;
    assert!(matches!(result, HookOutcome::InjectMessage { severity: HookSeverity::Warning, .. }));
}

#[tokio::test]
async fn command_hook_parses_json_outcome() {
    let hook = CommandHook { ..., command: r#"echo '{"InjectMessage":{"content":"hi","severity":"Info"}}'"#.into() };
    let result = hook.execute(...).await;
    assert!(matches!(result, HookOutcome::InjectMessage { content, .. } if content == "hi"));
}

#[tokio::test]
async fn guardrail_hook_wraps_existing_guardrail() {
    // 验证 GuardrailHook 在 Stop phase 复用现有 run_guardrails 逻辑
    let h = GuardrailHook::new(vec![Guardrail::ArtifactExists("foo.txt".into())]);
    let ctx = mock_ctx(HookPhase::Stop);  // workspace 没 foo.txt
    if let HookOutcome::InjectMessage { content, severity: HookSeverity::Blocking, .. } = h.execute(&ctx).await {
        assert!(content.contains("artifact"));
    } else { panic!() }
}
```

### 4.2 集成测试

```rust
async fn test_hook_inject_message_makes_agent_continue() {
    // 一个 Stop hook 在第一次 task_complete 时 inject "you forgot X"
    // 期望：agent 不 completed，注入 X 后再 task_complete（第二次）才完成
    // 断言：events 含 hook_inject + 后续 step 完成
}

async fn test_hook_prevent_terminal_fails_agent() {
    // 一个 PostToolUse hook 返回 PreventContinuation { terminal: true, reason: "no" }
    // 期望：agent 直接 failed，events 含 hook_prevented(terminal)
}

async fn test_workspace_hooks_loaded_from_config() {
    // 在 mock workspace 写一个 .miragenty/hooks.json
    // 启动 agent，验证 hook 实际被注册并触发
}
```

### 4.3 手动 E2E

写一个 workspace `.miragenty/hooks.json`：

```json
{
  "PostToolUse": [
    {
      "matcher": "write_file|edit_file",
      "hooks": [{ "type": "command", "command": "cd src-tauri && cargo check --message-format=short 2>&1 | head -50" }]
    }
  ]
}
```

跑一个会改 Rust 代码的 mission，期望：

- 每次 write/edit_file 后 timeline 出现 `hook_executed` 事件
- 编译错时 hook InjectMessage 把错误塞回 conversation
- agent 自动尝试修复编译错

---

## 5. 风险

| 风险 | 缓解 |
| --- | --- |
| Command hook RCE（恶意 workspace 注入危险脚本） | 默认禁用 + 首次启用时显式 review + hash 变更弹 approval |
| Hook 自身耗时拖慢 agent | per-hook 60s 默认 timeout；events 显示 duration_ms 让用户察觉 |
| 大量 hook injection 把 context 撑爆 | 每个 phase 的 injection 总长度 cap（10KB），超长 truncate + 提示 |
| hook 写得不对触发死循环（InjectMessage 永不退出） | Stop hook injection 计数 cap：单 step 同 phase 同 hook 触发 ≥ 3 次 → 视为 PreventContinuation |
| 跨 OS（Windows）shell 命令兼容性 | 第一版仅 macOS/Linux；Windows 用 PowerShell hook（后续 PR） |

---

## 6. 落地清单

| 文件 | 变更 | 行数 |
| --- | --- | --- |
| `src-tauri/src/agent/hooks/mod.rs` | **新建**：trait + enums + 注册表 | ~250 |
| `src-tauri/src/agent/hooks/builtin.rs` | **新建**：GuardrailHook + 内置示例 | ~120 |
| `src-tauri/src/agent/hooks/command.rs` | **新建**：CommandHook | ~150 |
| `src-tauri/src/agent/hooks/config.rs` | **新建**：hooks.json 加载 | ~150 |
| `src-tauri/src/agent/hooks/tests.rs` | 9 单测 | ~250 |
| `src-tauri/src/agent/engine.rs` | (a) hook_registry 字段; (b) 7 处 phase 调用点; (c) PreventContinuation 处理; (d) 3 集成测试 | ~300 |
| `src-tauri/src/db/migrations.rs` | migration 25: agent_events.kind 加 hook_executed/hook_inject/hook_prevented | ~10 |
| `src-tauri/src/commands/config.rs` | `allow_command_hooks: bool` | ~10 |
| `src/views/SettingsView.tsx` | Toggle + Workspace hooks review modal | ~80 |
| `src/components/workspace/EventList.tsx` | hook_* 事件渲染 | ~30 |
| `src/i18n/locales/{en-US,zh-CN}.json` | settings.hooks.* + events.hooks.* | ~15 |

合计 ~1370 行。**P2 中最重的一条，独立 sprint 跑**。

### commit 计划（拆 5 个 commit）

```
feat(agent): introduce generic hook trait and registry

Define AgentHook trait, HookOutcome (Pass/Inject/Prevent), HookContext
with phase + summarised messages/tool-use info, and a phase-aware
registry that short-circuits on Prevent and aggregates Inject.

This is foundation only — engine integration and concrete hooks land
in follow-up commits.
```

```
feat(agent): wrap existing Guardrail as a builtin Stop hook

Re-implement Guardrail invocation as a HookKind::Builtin so the engine
goes through the unified hook path. Zero behaviour change; sets up the
substrate for user-defined hooks in subsequent commits.

Module: FM-02
```

```
feat(agent): integrate hook phases into engine main loop

Invoke hook registry at PreLlmCall, PostSampling, PostToolUse,
PreCompact, PostCompact, Stop, and TaskCompleted boundaries. Inject
messages flow back into conversation; PreventContinuation forces
step/agent termination as configured.
```

```
feat(agent): support command-type hooks loaded from .miragenty/hooks.json

Load workspace-level hooks.json when allow_command_hooks is enabled in
settings. Commands receive JSON HookContext over stdin and can return
either a JSON HookOutcome or rely on exit code semantics. Default
60s timeout with per-hook override.
```

```
feat(ui): expose hook permission toggle and timeline rendering
```

---

## 7. 与其它优化项的关系

- **P0-1 / P0-3 (recovery)**：reactive compact 触发对应 PreCompact / PostCompact hook；如果用户配了 hook，恢复路径也走 hook（用户可监听）
- **P0-2 (token budget)**：budget exhausted 时可作为 Stop hook 的预设；用户能用 hook 实现自定义"budget 达到 X% 时跑某动作"
- **P1-1 (streaming tool execution)**：PostToolUse hook 在 drain_in_order 之后逐工具触发（不在 spawn 时）
- **现有 Approval Gate**：可以视为 PreToolUse phase 的内置实现（未来重构成 hook，本 PR 不动）

---

## 8. 不在本 PR 内

1. **LlmEvaluator hook 类型**：用小模型当 hook（"问问看代码对吗"），需要 prompt template 系统
2. **Hook 之间数据传递**：A hook 输出给 B hook 当输入，要 channel / store
3. **Hook 跨 agent 共享 state**：multi-agent 协作时的 hook 总结，与 multi-agent scheduler 相关
4. **可视化 Hook 编辑器**：UI 拖拽配置 hook，简易模板库
