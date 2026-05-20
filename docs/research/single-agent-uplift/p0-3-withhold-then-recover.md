# P0-3：Withhold-then-Recover —— 可恢复错误不让用户看见

> **目标**：把"prompt_too_long / max_output_tokens / media_size" 这类**确定性可恢复**错误从"立刻 emit error 事件 + 可能直接 fail"改成"先扣下错误 → 跑恢复流程 → 恢复成功就当没发生 → 恢复失败再 surface"。
>
> **对标**：Claude Code `query.ts:861-895`（withhold 阶段）+ `1138-1340`（recover 阶段）。
> **本系列定位**：单 agent 鲁棒性升级 7 篇之 3。前置依赖：P0-1 (Reactive Compact)。

---

## 1. 现状

### 1.1 Miragenty 当前错误处理（暴露每一个错误）

| 错误类型 | 当前位置 | 当前行为 | 用户感知 |
| --- | --- | --- | --- |
| `prompt_too_long` | `engine.rs:928` | `return Err` → step fail（P0-1 改造后变成 reactive compact retry） | 改造前：error 事件 + agent failed |
| `max_output_tokens` / `stop_reason == "length"` | `engine.rs:1035-1079` | emit `system_hint` 事件 + 注入"请拆分"提示 + 继续 | 看到一条 system_hint，知道刚撞顶了 |
| `IdleTimeout` | `engine.rs:892-923` | emit `system_hint` + 注入 "[System] 上一次响应中断" + continue | 看到 hint 后 LLM 续写 |
| 网络 transient | `stream_guard.rs:262-274` | `StreamRetryPolicy` 退避重试，每次 warn 日志 | 不可见（除非 retry budget 耗尽） |
| `auth` / `rate_limit` / unknown | `engine.rs:928` | 直接 fail | error 事件 + agent failed |

**问题**：可恢复错误（idle/max_tokens/prompt_too_long）每次发生都打扰用户。一个长 session 跑下来可能 emit 10+ 条 system_hint，用户看到一堆"出错了但好了"的事件，干扰真正需要关注的信号。

### 1.2 Claude Code 的双层处理

```typescript
// query.ts:861-895 - 流式阶段 "withhold"
let withheld = false
if (contextCollapse?.isWithheldPromptTooLong(message, ...)) withheld = true
if (reactiveCompact?.isWithheldPromptTooLong(message)) withheld = true
if (reactiveCompact?.isWithheldMediaSizeError(message)) withheld = true
if (isWithheldMaxOutputTokens(message)) withheld = true

if (!withheld) {
  yield yieldMessage   // 真错误才让前端看到
}
// withheld 的错误仍 push 到 assistantMessages，给后续 recovery 流程检索
```

```typescript
// query.ts:1138-1340 - 流后 "recover"
// 1) prompt_too_long → collapse drain → reactive compact → 救不回来才 yield 真错误
// 2) max_output_tokens → 升级到 64K → recovery loop → 救不回来才 surface
// 3) media_size → strip image → reactive compact → 救不回来才 surface
```

**核心机制**：错误分两类——**"我能自救的"** 和 **"用户必须知道的"**。前者发生时**完全不打扰用户**（连日志都是 debug 级），救成功就什么也没发生；只有真救不了才升级成用户可见事件。

---

## 2. 目标行为

### 2.1 三个 "可静默恢复" 通道

| 错误 | 静默恢复机制 | 已存在 / 新增 |
| --- | --- | --- |
| `prompt_too_long` | P0-1 reactive compact | **依赖 P0-1**（已设计） |
| `IdleTimeout` | 现有 idle_retry_budget 续写 | **已存在**（现状） |
| `max_output_tokens` / `length` | P1-3 升级式重试（先升 64K 再 multi-turn recovery） | **依赖 P1-3** |

P0-3 不新增恢复机制本身，**只统一"静默"语义**：把上面三类的"成功恢复"变成完全静默，"恢复失败"才 emit 用户可见事件。

### 2.2 静默 vs 可见的分界

| 阶段 | 静默信号 | 可见信号 |
| --- | --- | --- |
| 错误发生 | `tracing::debug!`（log 文件可查） | ❌ 不 emit event |
| 恢复尝试 | `tracing::info!` + emit `recovery_attempt` event（带 `silent: true` meta，前端默认不渲染） | ❌ 不弹 banner |
| 恢复成功 | `tracing::info!` + emit `recovery_succeeded` event（同 silent） | ❌ |
| 恢复失败 | `tracing::error!` + emit `error` event 走旧路径 | ✅ 红色 error |

### 2.3 与现状的兼容

- 现有 `system_hint` 事件**保持向后兼容**：旧调用点（read-only loop、no-tool hint 等）继续走 emit。
- 新增 `recovery_attempt` / `recovery_succeeded` 事件**默认前端不渲染**，调试模式（Settings → Developer → Show Recovery Events）下渲染为灰色细条目。
- 这条原则等于把 "无意义的恢复噪音" 默认隐藏，但所有原始数据 100% 落库可审计。

---

## 3. 设计细节

### 3.1 新增事件类型

`agent_events.kind` CHECK 约束追加两个值（迁移）：

```sql
-- migration 23
ALTER TABLE agent_events DROP CONSTRAINT agent_events_kind_check;
ALTER TABLE agent_events ADD CONSTRAINT agent_events_kind_check
  CHECK (kind IN (
    -- ... 现有 kinds
    'recovery_attempt',
    'recovery_succeeded'
  ));
```

事件 schema：

```jsonc
{
  "kind": "recovery_attempt",
  "content": "Auto-recovery for prompt_too_long: reactive compact (drop 12 messages)",
  "meta": {
    "silent": true,                       // 前端默认不渲染
    "trigger": "prompt_too_long",         // prompt_too_long | max_output_tokens | idle_timeout
    "strategy": "reactive_compact",       // reactive_compact | idle_retry | output_tokens_escalate | output_tokens_continue
    "error_excerpt": "...",
    "attempt": 1                          // 同一 step 内第几次尝试
  }
}

{
  "kind": "recovery_succeeded",
  "content": "Recovered from prompt_too_long via reactive compact (89K → 41K tokens)",
  "meta": {
    "silent": true,
    "trigger": "prompt_too_long",
    "strategy": "reactive_compact",
    "details": { /* strategy-specific */ }
  }
}
```

### 3.2 统一的 Recovery Logger

新增 `agent/recovery_log.rs`：

```rust
//! 统一的 "可恢复错误" 日志器。
//!
//! 调用点：engine.rs 主循环里所有"识别 + 尝试恢复"的路径都通过这两个 fn
//! 落 event，保证：
//!   - 一致的 meta schema（前端只需一个 renderer）
//!   - silent flag 统一加，避免漏 meta 一处就破坏体感
//!   - 后续要做 "recovery 历史聚合" 时有规整数据

pub enum RecoveryTrigger {
    PromptTooLong,
    MaxOutputTokens,
    IdleTimeout,
    MediaSizeError,        // 留位，未启用
}

impl RecoveryTrigger {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::PromptTooLong => "prompt_too_long",
            Self::MaxOutputTokens => "max_output_tokens",
            Self::IdleTimeout => "idle_timeout",
            Self::MediaSizeError => "media_size_error",
        }
    }
}

pub enum RecoveryStrategy {
    ReactiveCompact { dropped_msgs: usize, tokens_before: usize, tokens_after: usize },
    IdleRetryContinue { retries_left: u32 },
    OutputTokensEscalate { old_cap: u32, new_cap: u32 },
    OutputTokensContinue { recovery_count: u32 },
    ImageStrip { stripped_count: u32 },
}
```

提供两个 helper：

```rust
impl super::AgentEngine {
    pub(crate) fn emit_recovery_attempt(
        &self,
        agent_id: &str,
        step: u32,
        trigger: RecoveryTrigger,
        strategy: RecoveryStrategy,
        error_excerpt: &str,
        attempt: u32,
    ) {
        let content = format!("Auto-recovery for {}: {}", trigger.as_str(), strategy.human_label());
        let meta = serde_json::json!({
            "silent": true,
            "trigger": trigger.as_str(),
            "strategy": strategy.kind_label(),
            "error_excerpt": error_excerpt.chars().take(200).collect::<String>(),
            "attempt": attempt,
            "details": strategy.details_json(),
        });
        self.emit_event_with_meta(agent_id, step, "recovery_attempt", &content, Some(meta));
    }

    pub(crate) fn emit_recovery_succeeded(
        &self,
        agent_id: &str,
        step: u32,
        trigger: RecoveryTrigger,
        strategy: RecoveryStrategy,
    ) {
        let content = format!("Recovered from {}: {}", trigger.as_str(), strategy.human_label());
        let meta = serde_json::json!({
            "silent": true,
            "trigger": trigger.as_str(),
            "strategy": strategy.kind_label(),
            "details": strategy.details_json(),
        });
        self.emit_event_with_meta(agent_id, step, "recovery_succeeded", &content, Some(meta));
    }
}
```

### 3.3 现有调用点迁移

#### 3.3.1 P0-1 (Reactive Compact) 集成

P0-1 的 emit_event_with_meta 调用替换：

```rust
// 旧（P0-1 设计）：
self.emit_event_with_meta(agent_id, step, "compact", &human, Some(meta));

// 新（统一走 recovery_log）：
self.emit_recovery_attempt(agent_id, step, RecoveryTrigger::PromptTooLong,
    RecoveryStrategy::ReactiveCompact { ... }, &msg, 1);
// retry 成功后（流正常返回）：
self.emit_recovery_succeeded(agent_id, step, RecoveryTrigger::PromptTooLong, RecoveryStrategy::ReactiveCompact { ... });
```

如果 reactive compact 失败（messages 太少压不动）→ 走原 error 路径（红色可见）。

#### 3.3.2 IdleTimeout 迁移

```rust
// 现 engine.rs:892-923
Err(StreamGuardError::IdleTimeout { idle_secs, threshold_secs }) if idle_retries_left > 0 => {
    idle_retries_left -= 1;
    resume_after_idle_retry = true;
    // 旧：emit system_hint（用户可见）
    // 新：emit recovery_attempt（silent）
    self.emit_recovery_attempt(
        agent_id, step, RecoveryTrigger::IdleTimeout,
        RecoveryStrategy::IdleRetryContinue { retries_left: idle_retries_left },
        &format!("idle {idle_secs}s > {threshold_secs}s"), 1
    );
    messages.push(/* "[System] 上一次响应在 Xs 后中断..." */);
    continue;
}
```

下一 step 成功开始后（拿到 first chunk）→ emit_recovery_succeeded。

**实现要点**：需要在 forwarder task 拿到 first chunk 时回调 main loop——可以用一个 oneshot channel：retry 后注册 oneshot，forwarder 拿到 first chunk 时 send，main loop 在 stream_outcome 成功后 try_recv 决定是否 emit succeeded。

简化方案：不追求"严格 succeeded"语义，而是"下一次 step 成功完成 LLM 调用即视为 succeeded"——在 stream_outcome OK 分支检查 `resume_after_idle_retry` flag 是否在上一轮被 set，是则 emit succeeded。

#### 3.3.3 max_output_tokens 迁移

现 `engine.rs:1035-1079` 路径（emit system_hint + 注入拆分提示）整体保留，但 emit 走 recovery_log：

```rust
if response.stop_reason == "length" || response.stop_reason == "max_tokens" {
    // 新：先 emit recovery_attempt
    self.emit_recovery_attempt(
        agent_id, step, RecoveryTrigger::MaxOutputTokens,
        RecoveryStrategy::OutputTokensContinue { recovery_count: 1 },
        &format!("stop_reason={}", response.stop_reason), 1,
    );
    // ... 现有 hint 注入逻辑保留
}
```

**与 P1-3 (max_output_tokens 升级式重试) 的关系**：P1-3 会加 escalate 路径（first try 16K → next try 64K → 再用 multi-turn recovery）。三个阶段都走 `emit_recovery_attempt`，只是 strategy 不同（`OutputTokensEscalate` / `OutputTokensContinue`）。

### 3.4 前端最小改动

`src/components/workspace/EventList.tsx`（或同类渲染组件）：

```typescript
function shouldRenderEvent(ev: AgentEvent, showRecoveryEvents: boolean): boolean {
  if (ev.kind === 'recovery_attempt' || ev.kind === 'recovery_succeeded') {
    const silent = (ev.meta as any)?.silent === true;
    return !silent || showRecoveryEvents;
  }
  return true;
}
```

`Settings` 加 toggle `showRecoveryEvents` (默认 false)。

存储侧：所有 recovery 事件**仍持久化到 DB**，只是 UI 默认隐藏。用户开发者模式 / 导出 diagnostic bundle 时能看到完整链路。

---

## 4. 验收

### 4.1 单元测试（`recovery_log.rs`）

```rust
#[test]
fn strategy_label_matches_meta() {
    let s = RecoveryStrategy::ReactiveCompact { dropped_msgs: 12, tokens_before: 80000, tokens_after: 40000 };
    assert_eq!(s.kind_label(), "reactive_compact");
    assert!(s.human_label().contains("reactive compact"));
    let json = s.details_json();
    assert_eq!(json["dropped_msgs"], 12);
}
```

### 4.2 集成测试

```rust
async fn test_idle_timeout_recovery_emits_silent_attempt_and_succeeded() {
    // StubProvider：step 1 idle 后续写成功
    // 断言：events 包含 recovery_attempt(silent=true) + recovery_succeeded(silent=true)
    // 断言：events **不包含** system_hint with idle 关键词（防回归）
}

async fn test_prompt_too_long_recovery_silent_when_succeeded() {
    // StubProvider：step 3 报 context_too_long，retry 时正常
    // 断言：events 包含 recovery_attempt + recovery_succeeded，meta.silent=true
}

async fn test_prompt_too_long_recovery_surfaces_when_failed() {
    // StubProvider：step 3 报 context_too_long，且 messages 太短无法压
    // 断言：events 含一条 error 事件（可见），不含 recovery_succeeded
}
```

### 4.3 前端

vitest 单测 `shouldRenderEvent`：
- silent recovery + dev mode off → not rendered
- silent recovery + dev mode on → rendered
- non-silent recovery → 应该不存在这种情况，但即使存在也 rendered

### 4.4 手动 E2E

跑一个会触发 idle / context_too_long 的长 mission：

- 默认设置下：workspace timeline 里不应有任何 recovery 噪音；只在真出问题时看到红色 error
- 开 dev mode：看到灰色细条目记录每次 recovery 尝试

---

## 5. 风险

| 风险 | 缓解 |
| --- | --- |
| 静默后用户不知道 LLM 在"暗中重试" → 看到的 token cost 突然飙升却没事件 | 在 mission detail 页加一个 "Recoveries: N" 计数（不展开细节），让用户感知量级而不被噪音轰炸 |
| 真错误被错误归到"可恢复"了 → 默默吞掉用户应该知道的问题 | 三个分类的判定都来自现有路径（不新增），现有路径在恢复 budget 耗尽时仍会走 error → 用户兜底能看到 |
| recovery_succeeded 事件需要回调机制 | 用 oneshot channel + 简化语义（下次 LLM 调用 OK 即视为成功） |
| migration 23 改 CHECK 约束影响老 mission 数据加载 | CHECK 约束只对新插入生效，旧数据不受影响。回归测试覆盖 |

---

## 6. 落地清单

| 文件 | 变更 | 行数 |
| --- | --- | --- |
| `src-tauri/src/db/migrations.rs` | 新增 migration 23 改 CHECK 约束 | ~15 |
| `src-tauri/src/agent/recovery_log.rs` | **新建**：enum + 两个 emit helper + 1 单测 | ~150 |
| `src-tauri/src/agent/mod.rs` | `mod recovery_log; pub(crate) use recovery_log::*;` | 2 |
| `src-tauri/src/agent/engine.rs` | 3 处调用点迁移（P0-1 compact / idle retry / max_tokens hint） | ~50 |
| `src-tauri/src/agent/engine.rs` 测试 | 3 个集成测试 | ~100 |
| `src/stores/agent-store.ts` | `AgentEvent['kind']` 加两值 | ~5 |
| `src/components/workspace/*` | `shouldRenderEvent` filter + vitest 单测 | ~40 |
| `src/views/SettingsView.tsx` | Developer toggle `showRecoveryEvents` + i18n | ~30 |
| `src/i18n/locales/{en-US,zh-CN}.json` | settings.developer.show_recovery_events | ~6 |

合计 ~400 行。

### commit 计划

```
feat(db): add migration 23 for recovery_attempt/succeeded event kinds
```

```
feat(agent): add silent recovery logger for in-step error recovery

Wrap idle-timeout retry, reactive compact, and max_output_tokens
recovery in a unified emit_recovery_{attempt,succeeded} helper that
marks events as silent. Frontend hides silent events behind a
Developer setting, surfacing only true unrecoverable errors.

Module: FM-04, FM-02
```

```
feat(ui): hide silent recovery events behind Developer setting
```

```
test(agent): cover recovery logger silence semantics end-to-end
```

---

## 7. 与其它 P0/P1 的关系

| 依赖项 | 关系 |
| --- | --- |
| **P0-1 Reactive Compact** | 前置依赖：P0-3 把 P0-1 的 emit 路径包成 recovery_attempt/succeeded |
| **P1-3 max_output_tokens 升级** | 协同：P1-3 实现升级策略，P0-3 提供 emit 通道 |
| **P1-2 Cross-model Fallback** | 协同：fallback 触发也可走 recovery_log 的 silent 通道（避免每次切模型都打扰） |
| **P2-1 Stop Hook** | 远期：未来 hook 可订阅 recovery_succeeded 事件做统计 |
