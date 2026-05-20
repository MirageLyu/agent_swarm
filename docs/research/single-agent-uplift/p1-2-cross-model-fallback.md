# P1-2：Cross-Model Fallback —— overload / rate-limit 时自动切到备用模型

> **目标**：识别"主模型暂时不可用"（overload / rate-limit），自动切到预配置的 fallback 模型重发请求，让长 mission 不因一次性高峰失败。
>
> **对标**：Claude Code `query.ts:967-1024` 的 `FallbackTriggeredError` 处理。
> **本系列定位**：单 agent 鲁棒性升级 7 篇之 5。前置依赖：P0-3 (recovery_log 通道，optional)。

---

## 实施状态（2026-05-20 更新）

### Phase A（已完成 ✅）—— 核心后端逻辑 + 单测

落地内容：
1. **`llm/error_class.rs`**：`LlmErrorClass` 加 `Overloaded` / `RateLimited` 变体 + `is_fallback_trigger()` 方法；新增 Anthropic 529 / OpenAI 503 / 429 / quota_exceeded 关键词集合；优先级 PromptTooLong > RateLimited > Overloaded > Generic（含 7 个新单测覆盖）
2. **`agent/fallback.rs`**（新文件）：
   - `strip_reasoning_blocks` helper：跨模型前清除 Reasoning content（thinking signature 跨模型不通用，不剥会触发 400）
   - `FallbackSwitchMeta` struct：标准化 event metadata schema
   - 5 个单测覆盖 strip 行为 + meta 序列化稳定性
3. **`agent/recovery_log.rs`**：`RecoveryTrigger` 加 `Overloaded` / `RateLimited`；`RecoveryStrategy` 加 `ModelFallback { from, to, switch_total }`；`from_error_class` 映射 helper
4. **`agent/engine.rs`**：
   - `AgentRunOptions` 加 `fallback_model: Option<String>` + `fallback_sticky: bool`（默认 `None` + `true`，向后兼容）
   - run_inner 加 `current_model` / `switched_to_fallback_this_step` / `fallback_switches_total` state
   - `LlmRequest.model` 改用 `current_model`（不是 `opts.model`）
   - 新增 `Err(StreamGuardError::Llm)` 分支：检测 fallback trigger + 未在本 step 切过 + 配置了不同 fallback_model → 切换 + strip reasoning + emit 双事件（silent `recovery_attempt` + 可见 `system_hint`）+ `continue` 重发同 step
   - step 边界（步号递增时）reset `switched_to_fallback_this_step`，并按 `fallback_sticky=false` 逻辑回切 primary
5. **`agent/scheduler.rs`**：构造 `AgentRunOptions` 时初始化新字段为默认值

测试结果：529 全过（+14 自 P1-1 Phase A 完成时的 515）。0 行为变化（默认 `fallback_model=None` → 旧路径完全不变）。

### Phase B（已完成 ✅）—— Settings UI / AppConfig / DB / Report

落地内容：
1. **Migration 027**：`agents.fallback_switches_total INTEGER NOT NULL DEFAULT 0`
2. **`engine.rs::persist_fallback_switches`** helper：每次 fallback 切换发生时立即 write-through 持久化（非延后到 agent 结束，防 agent 在 fallback 后崩溃导致计数丢失）
3. **`report_generator.rs`** `ReportMetrics.fallback_switches_total`：SUM(agents.fallback_switches_total) 聚合；markdown render 仅在 >0 时输出 `| Model fallbacks | N |` 行避免 0 值噪音
4. **`AppConfig.agent_fallback_model: String` + `agent_fallback_sticky: bool`**：empty string = 关闭，默认 sticky=true。`update_config` clamp + trim
5. **`scheduler.rs::build_agent_run_options`** plumb cfg → AgentRunOptions（取代 `None`/`true` 硬编码）
6. **`SettingsView.tsx` Cross-Model Fallback section**：model input + sticky segmented control + i18n（en/zh），含 RCE/cost 警告说明

测试结果：541 → 562 全过；前端 MissionReportMetrics IPC 接口同步加 `fallback_switches_total: number` 字段。

---

---

## 1. 现状

### 1.1 当前重试策略（单模型）

`engine.rs:855-868`：

```rust
let retry_policy = StreamRetryPolicy {
    max_retries: opts.stream_network_retries,   // 默认 5
    initial_backoff: Duration::from_millis(opts.stream_initial_retry_delay_ms),
    max_backoff: Duration::from_secs(16),
};
let stream_outcome = stream_chat_with_idle_guard_full(
    self.provider.clone(),
    request,
    tx,
    DEFAULT_STREAM_IDLE_TIMEOUT,
    self.cancel_token.clone(),
    retry_policy,
).await;
```

`StreamRetryPolicy`（`stream_guard.rs:243-277`）仅在**已收到 0 个 chunk**时重试同模型，且只对 `StreamGuardError::Llm` 触发。生产场景的实际反应：

1. Anthropic overload（HTTP 529）→ `bail!("API error 529: overloaded_error")` → StreamGuardError::Llm → retry 5 次（1s/2s/4s/8s/16s 共 31s）→ 全失败 → agent failed
2. Rate limit (HTTP 429) → 同样路径 → 5 次内大概率 hit 同样错误 → agent failed
3. 模型暂时下线（reseller 故障）→ 同样 → agent failed

**单次 retry 周期 ≈ 31s**，期间用户看到的只有日志里的 warn，UI 默认无可见信号。

### 1.2 Claude Code 怎么处理

```typescript
// services/api/withRetry.ts 内部识别 overload / rate-limit 类错误
// → throw FallbackTriggeredError(originalModel, fallbackModel)
// query.ts:967-1024 接住这个错误：
catch (innerError) {
    if (innerError instanceof FallbackTriggeredError && fallbackModel) {
        currentModel = fallbackModel;
        attemptWithFallback = true;

        // 清掉已收到的 partial 内容（避免拼接错乱）
        yield* yieldMissingToolResultBlocks(assistantMessages, 'Model fallback triggered');
        assistantMessages.length = 0;
        toolResults.length = 0;

        // **关键**：剥离 thinking signatures，跨 model 不兼容
        if (process.env.USER_TYPE === 'ant') {
            messagesForQuery = stripSignatureBlocks(messagesForQuery);
        }

        // 用户可见提示
        yield createSystemMessage(
            `Switched to ${renderModelName(fallbackModel)} due to high demand for ${renderModelName(originalModel)}`,
            'warning',
        );
        continue;
    }
    throw innerError;
}
```

**几个关键设计点**：
1. **触发判定下沉到 retry 层**：retry 包装识别"这个错误是切 fallback 的信号"，而不是 query 主循环判断
2. **清空 partial**：avoid 跨 model 拼回半截 assistant message
3. **strip thinking signatures**：thinking 块（reasoning content）有 model-specific 签名，跨 model 重发会 400
4. **用户可见但不打扰**：emit 一条 warning 级 system message，不当 error 处理

---

## 2. 目标行为

### 2.1 错误分类升级

在 P0-1 设计的 `LlmErrorClass` 上加两个变体：

```rust
pub enum LlmErrorClass {
    PromptTooLong,        // P0-1 已加
    /// 模型 overload（503 / 529 等）：上游临时不可用，切 fallback 模型大概率立刻成功
    Overloaded,
    /// Rate limit（429）：accountant/key 维度限速，同样切 fallback 有效
    RateLimited,
    /// 其它
    Generic,
}
```

分类器扩展（错误消息匹配）：

```rust
const OVERLOAD_NEEDLES: &[&str] = &[
    "overloaded_error",        // Anthropic 标准 code
    "overloaded",
    "service_unavailable",      // OpenAI 503
    "model_overloaded",
    "capacity",                 // 较多 reseller 用
    "try again later",          // 通用兜底（与 rate limit 重叠，归 overloaded 处理也 OK）
];
const RATE_LIMIT_NEEDLES: &[&str] = &[
    "rate_limit_exceeded",      // OpenAI 标准 code
    "too many requests",
    "request_limit",
    "quota_exceeded",            // 余额/quota 不够也按 fallback 处理（fallback 可能用不同 key）
];
```

### 2.2 触发逻辑（engine.rs 主循环）

```
[stream_outcome = Err(StreamGuardError::Llm(msg))]
     │
     ├── classify_llm_error(msg) == PromptTooLong → 走 P0-1 reactive compact 分支
     │
     ├── classify_llm_error(msg) ∈ {Overloaded, RateLimited} 且 has_fallback_model:
     │     │
     │     ├── 已经在本 step 切过 fallback 一次 → 不再切，按 generic error 处理 fail
     │     │
     │     └── 第一次切：
     │           ├── emit recovery_attempt(trigger=Overloaded/RateLimited,
     │           │       strategy=ModelFallback, from=primary, to=fallback)
     │           ├── current_model = opts.fallback_model
     │           ├── messages = strip_reasoning_blocks(messages)
     │           ├── set switched_to_fallback_this_step = true
     │           ├── emit 一条 user-visible system_hint（不静默，因为模型变了用户得知道）：
     │           │       "Primary model overloaded, switched to {fallback}. Retrying."
     │           └── continue（同 step 重发，但用 fallback model）
     │
     └── 其它 Llm 错误 → 现状（fail）
```

### 2.3 一些约束

- **fallback 不递归**：fallback 模型如果也 overload → 按 generic error fail，不再切回 primary 或切第三个
- **per-step 标记，不跨 step**：下个 step 开始时 `switched_to_fallback_this_step` 重置；但 `current_model` 是否回切 primary 是配置问题（见 §3.4）
- **task_complete 时的报告**：mission report 里要能看到"本 mission 触发过 N 次 fallback，从 X 切到 Y"

---

## 3. 设计细节

### 3.1 AgentRunOptions 升级

```rust
pub struct AgentRunOptions {
    pub model: String,
    /// 可选备用模型；overload/rate-limit 时切到此模型重发当前 step。
    /// None = 关闭 fallback，行为同现状。
    pub fallback_model: Option<String>,
    /// fallback 触发后：
    ///   - true: 后续 step 继续用 fallback（默认）
    ///   - false: 下 step 重新尝试 primary
    /// "stick" 默认 true 因为 overload 通常持续几分钟，频繁切换浪费成本
    pub fallback_sticky: bool,
    // ... 现有字段
}
```

### 3.2 strip_reasoning_blocks（新工具函数）

跨 model 时 reasoning（thinking）签名不兼容。`ContentBlock::Reasoning { text }` 块全部 drop：

```rust
/// 跨 model fallback 前调用：移除所有 Reasoning 块。
///
/// 原因：reasoning_content 在 DeepSeek-R1/V4 / Anthropic thinking 等模型间不通用，
/// 把上一个 model 的 thinking 喂给 fallback 会触发 400 "thinking signature mismatch"
/// 或被 fallback 不识别字段直接报错。
///
/// 影响：丢失 thinking 上下文。但这是为了**完成任务** vs **保留 thinking**的取舍——
/// 选完成任务。reasoning 内容已经在 events 里持久化，需要时可查。
pub fn strip_reasoning_blocks(messages: &mut Vec<Message>) {
    for msg in messages.iter_mut() {
        msg.content.retain(|b| !matches!(b, ContentBlock::Reasoning { .. }));
    }
}
```

### 3.3 主循环改造点

```rust
// run_inner 函数开头
let mut current_model = opts.model.clone();
let mut switched_to_fallback_this_step = false;
let mut fallback_switches_total: u32 = 0;

// loop 顶部
switched_to_fallback_this_step = false;

// 构造 request 时
let request = LlmRequest {
    model: current_model.clone(),     // ← 用 current_model 而非 opts.model
    // ...
};

// stream_outcome 错误分支扩充
Err(StreamGuardError::Llm(msg)) => {
    use crate::llm::error_class::{classify_llm_error, LlmErrorClass};
    let class = classify_llm_error(&msg);

    // ① PromptTooLong → P0-1 路径
    if class == LlmErrorClass::PromptTooLong && !attempted_reactive_compact_this_step {
        attempted_reactive_compact_this_step = true;
        // ... P0-1 reactive compact 路径
        continue;
    }

    // ② Overloaded / RateLimited + 有 fallback 配置 → 切 fallback
    let is_fallback_trigger = matches!(class, LlmErrorClass::Overloaded | LlmErrorClass::RateLimited);
    if is_fallback_trigger
        && opts.fallback_model.is_some()
        && !switched_to_fallback_this_step
        && current_model != *opts.fallback_model.as_ref().unwrap()  // 防自切自
    {
        switched_to_fallback_this_step = true;
        fallback_switches_total += 1;
        let from_model = current_model.clone();
        let to_model = opts.fallback_model.clone().unwrap();
        current_model = to_model.clone();

        // strip reasoning
        strip_reasoning_blocks(&mut messages);

        // emit silent recovery_attempt + 可见 system_hint
        self.emit_recovery_attempt(
            agent_id, step,
            RecoveryTrigger::from_error_class(class),
            RecoveryStrategy::ModelFallback { from: from_model.clone(), to: to_model.clone() },
            &msg, 1,
        );
        let visible_msg = format!(
            "Primary model `{from_model}` is overloaded/limited; switched to fallback `{to_model}` and retrying this step."
        );
        self.emit_event_with_meta(
            agent_id, step, "system_hint", &visible_msg,
            Some(serde_json::json!({
                "kind": "model_fallback",
                "from": from_model,
                "to": to_model,
                "trigger": class.as_str(),
            })),
        );
        continue;
    }

    // ③ 其它 / fallback 已切过 → 原 fail 路径
    return Err(anyhow::anyhow!(msg));
}
```

### 3.4 sticky vs non-sticky 行为

```rust
// 在每 step 末尾（task_complete 之前 / 完成 follow-up 之后）
if !opts.fallback_sticky && current_model != opts.model && switched_to_fallback_this_step {
    // 非粘性：下 step 尝试切回 primary
    current_model = opts.model.clone();
}
// 粘性：保持在 fallback 直到 agent 结束
```

**默认 sticky=true**：reseller overload 一般持续 5-30 分钟，频繁切回 primary 等于反复撞墙，成本高。用户能在 Settings 关掉粘性。

### 3.5 Mission Report 集成

`agent/report_generator.rs` 在 mission summary 阶段统计每 agent 的 `fallback_switches_total`：

- DB 加 agent 维度计数（`agents.fallback_switches` 列，via migration）
- engine.rs 在 agent 完成/失败时 `UPDATE agents SET fallback_switches = ?`
- report 渲染时若 sum > 0 加一段"Model fallback occurred N times (X → Y)"

### 3.6 与 P1-1 (Streaming Tool Execution) 的交互

fallback 触发时如果 streaming executor 已经 spawn 了 partial tool（input 半截）：

- 显式调 `executor.abort_all()` 清空
- fallback 重发后 executor 处于空状态启动

---

## 4. 验收

### 4.1 单元测试

```rust
#[test]
fn classify_overloaded_anthropic() {
    assert_eq!(
        classify_llm_error(r#"OpenAI compat API error 529: {"error":{"type":"overloaded_error"}}"#),
        LlmErrorClass::Overloaded
    );
}

#[test]
fn classify_rate_limit_openai() {
    assert_eq!(
        classify_llm_error(r#"OpenAI compat API error 429: rate_limit_exceeded"#),
        LlmErrorClass::RateLimited
    );
}

#[test]
fn strip_reasoning_removes_only_reasoning() {
    let mut msgs = vec![Message {
        role: MessageRole::Assistant,
        content: vec![
            ContentBlock::Reasoning { text: "thinking...".into() },
            ContentBlock::Text { text: "answer".into() },
            ContentBlock::ToolUse { id: "x".into(), name: "y".into(), input: json!({}) },
        ],
        cache_control: None,
    }];
    strip_reasoning_blocks(&mut msgs);
    assert_eq!(msgs[0].content.len(), 2);
    assert!(matches!(msgs[0].content[0], ContentBlock::Text { .. }));
}
```

### 4.2 集成测试

```rust
async fn test_overload_triggers_fallback_switch() {
    // 双 provider：primary 总报 529 overloaded, fallback 正常
    // 期望：step 1 内 primary fail → switch → fallback 成功 → agent 继续
    // 断言：events 含一条 system_hint with meta.kind="model_fallback"
    // 断言：fallback_switches_total == 1
}

async fn test_only_one_fallback_per_step() {
    // primary overload, fallback 也 overload
    // 期望：切一次后 fallback fail → 整 agent fail（不再切第三个）
}

async fn test_no_fallback_when_not_configured() {
    // opts.fallback_model = None
    // primary overload → 直接 fail，没有 fallback 事件
}

async fn test_sticky_fallback_persists_across_steps() {
    // step 1 触发 fallback → step 2 直接用 fallback 不切回
    // 断言：step 2 的 llm_call meta.model == fallback_model
}

async fn test_non_sticky_returns_to_primary_next_step() {
    // opts.fallback_sticky = false
    // step 1 触发 fallback → step 2 用 primary（即便 primary 还会 overload）
}
```

### 4.3 手动 E2E

Mock 一个 reseller 在第 3 step 返回 529 → 期望 UI 看到"切到 fallback"通知，agent 继续完成。

---

## 5. 风险

| 风险 | 缓解 |
| --- | --- |
| Fallback 模型自己也限速 / 不稳 → 切了反而更糟 | 单 step 只切一次；用户可监控 fallback_switches_total |
| primary 和 fallback 价格差异大 → 用户没意识到成本飙升 | sticky 后下个 step 仍用 fallback，但 cost 走原 estimate_cost 流程会反映出来；mission report 显式列 fallback 次数 |
| 错误分类误判（generic transient 错误被当 overload） | 关键词集合保守，且 fallback 切错的代价是"换个 model 重发"——比 fail 整 agent 代价小一个数量级 |
| strip_reasoning 后 LLM 上下文不连贯 | 这是 unavoidable cost；reasoning 内容在 events 里仍可查 |
| 跨 provider fallback（Anthropic → DeepSeek）的 tool schema 差异 | 当前两家 schema 等价（OpenAI compat），未来跨 provider 时再处理；本 PR 仅同 provider 跨 model 切换 |

---

## 6. 落地清单

| 文件 | 变更 | 行数 |
| --- | --- | --- |
| `src-tauri/src/llm/error_class.rs` | 加 Overloaded/RateLimited + 关键词 + 5 单测（建立在 P0-1 之上） | ~70 |
| `src-tauri/src/llm/types.rs` | (可选) Reasoning 块的 helper 已无需新增 | 0 |
| `src-tauri/src/agent/fallback.rs` | **新建**：strip_reasoning_blocks + 1 单测 | ~30 |
| `src-tauri/src/agent/engine.rs` | (a) AgentRunOptions 加 fallback_model / fallback_sticky; (b) current_model 状态; (c) 错误分支切 fallback; (d) sticky 处理 | ~120 |
| `src-tauri/src/agent/engine.rs` 测试 | 5 集成测试 | ~200 |
| `src-tauri/src/db/migrations.rs` | migration 24: agents 加 fallback_switches INTEGER DEFAULT 0 | ~10 |
| `src-tauri/src/agent/scheduler.rs` | scheduler 从 mission config 读 fallback_model 注入 AgentRunOptions | ~30 |
| `src-tauri/src/agent/report_generator.rs` | mission report 渲染 fallback_switches > 0 时加段 | ~40 |
| `src-tauri/src/commands/config.rs` | AppConfig 加 agent_fallback_model / agent_fallback_sticky | ~20 |
| `src/views/SettingsView.tsx` | Fallback 模型选择 + sticky toggle + i18n | ~50 |
| `src/i18n/locales/{en-US,zh-CN}.json` | settings.fallback.* 4 keys | ~8 |

合计 ~580 行。

### commit 计划

```
feat(llm): classify overloaded and rate-limit errors

Extend LlmErrorClass with Overloaded and RateLimited variants for
upstream-busy signals from Anthropic 529, OpenAI 429, DeepSeek
overloaded responses. Foundation for cross-model fallback.
```

```
feat(agent): cross-model fallback on overload or rate-limit

When the primary model returns an overloaded/rate-limited error and
a fallback model is configured, switch within the same step and retry
once. Strip Reasoning blocks before the switch because thinking
signatures aren't portable across models.

Defaults to sticky (subsequent steps stay on fallback) since upstream
overload usually lasts minutes; can be disabled in Settings.

Module: FM-02
```

```
test(agent): cover fallback switch ordering, stickiness, and budget
```

```
feat(db): migration 24 add agents.fallback_switches counter
```

```
feat(commands,ui): expose fallback model + sticky toggle in Settings
```

```
feat(agent): include fallback switch count in mission report
```

---

## 7. 不在本 PR 内

1. **Provider 级 fallback**（Anthropic → DeepSeek 的跨厂商切换）：tool schema 差异要先处理
2. **自动 fallback 选模型**：现在用户配死一个 fallback；未来可以"按价格/速度阶梯自动选下一档"
3. **Fallback 反向回切**：fallback 用了 5 分钟后定期 ping primary 是否恢复
