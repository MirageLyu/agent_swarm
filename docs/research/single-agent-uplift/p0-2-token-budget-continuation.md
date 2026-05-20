# P0-2：Token Budget + 边际收益检测 —— 让 max_steps 不再"快完成时一脚踢飞"

> **目标**：把"agent 何时该停"从**单一硬上限 `max_steps`** 升级为**双信号**：「还有 token 预算 + 还在产出有效内容」就继续；「预算耗尽 OR 连续衰减」才停。
>
> **对标**：Claude Code 的 `query/tokenBudget.ts` + `query.ts:1384-1431` 的"token budget continuation"机制。
> **本系列定位**：单 agent 鲁棒性升级 7 篇之 2。

---

## 1. 现状（撞墙处）

### 1.1 硬上限的两种灾难

`engine.rs:595-606`：

```rust
if step >= opts.max_steps {
    let reason = format!("max_steps: {} steps exhausted without task_complete", opts.max_steps);
    self.emit_event(agent_id, step, "error", &reason);
    self.update_agent_status(agent_id, "failed");
    return Ok(AgentStatus::Failed);
}
```

**灾难 A：晚期失败（最典型）**

实测 trace（postmortem §4.3）：tester#2 跑到 step 78 时 LLM 已经在收尾，正打算调 `task_complete`——step 80 撞顶 fail。**整 agent 标 failed**，5.5M tokens 全废。第二次 retry cold-start 从 step 1 又走一遍同样路径。

**灾难 B：早期 loop（不那么明显）**

read-only loop 检测（`engine.rs:1192`）连续 5 步只读不写就注入一条 hint，但 LLM 经常无视。第 30 步还在 read 同样文件时，本质上**已经无效**，但硬上限要等到第 80 步才止损——中间 50 步空烧。

### 1.2 现有"补丁式"机制

| 机制 | 位置 | 局限 |
| --- | --- | --- |
| `STEPS_REMAINING_HINT` 收尾提示 | `engine.rs:608-625` | 一次性 hint，只在剩 5 步时打一次；LLM 经常理解成"还能写很久" |
| `MAX_CONSECUTIVE_NO_TOOL` | `engine.rs:1171` | 仅检测"光说不做"，不检测"做了但没进展" |
| `READ_ONLY_LOOP_THRESHOLD` | `engine.rs:1195-1218` | 仅检测"读但不改"；写了无用代码不触发 |
| `MICROCOMPACT_TOKEN_THRESHOLD = 50_000` | `engine.rs:186` | 触发的是 compact 不是 stop |

**共同缺陷**：所有信号都是**单向的**（要不让 LLM 加速，要不阻止 LLM 偷懒），**没有一个机制能根据"产出 token 数 vs 已花费 token 数"决定"够了，停吧"**。

### 1.3 Claude Code 是怎么干的

`query/tokenBudget.ts:45-93`：

```typescript
const isDiminishing =
  tracker.continuationCount >= 3 &&            // 已经续过 3 轮
  deltaSinceLastCheck < 500 &&                  // 本轮新增 < 500 token
  tracker.lastDeltaTokens < 500;                // 上轮也 < 500 token

if (!isDiminishing && turnTokens < budget * 0.9) {
  // 还有预算 + 仍在产出 → 注入 nudge user message 让 agent 继续
  return { action: 'continue', nudgeMessage: ... };
}
// 否则真正停
return { action: 'stop', completionEvent: ... };
```

核心信号：
1. **预算（绝对值）**：`budget * 0.9` ≈ 接近 turn 总额度
2. **产出（速率）**：连续 3 轮 < 500 token = 在原地打转

两个都满足才停。任何一个不满足 → 给一条 nudge 让它继续。

---

## 2. 目标行为

把 `max_steps` 从"硬上限"降格为"防御性上限"，引入**软上限**：

```
[step N 结束]
     │
     ├── LLM 调了 task_complete + guardrail 通过 → 真正完成
     │
     ├── step >= max_steps（硬上限）→ failed（现状不变，永远兜底）
     │
     ├── token_budget 未配置 → 维持现状（向后兼容）
     │
     └── token_budget 已配置：
            │
            ├── accumulated_output_tokens >= budget × 0.9
            │     │
            │     └── 连续 N 轮 delta < 500 (diminishing) → stop with reason "budget_exhausted"
            │
            └── accumulated_output_tokens < budget × 0.9 AND 距离 max_steps 还有空间
                  │
                  ├── 检测 diminishing returns（连续 3 轮 delta < 500）→ stop with reason "diminishing_returns"
                  │
                  └── 否则 → 不停，继续 loop（不需要注入 nudge —— 现 step 已经是 tool_use 跟着自然下一轮）
```

**关键差异**：Claude Code 是在 "no more tool_use" 边界（即 LLM 自然结束 turn）做 budget check + nudge；Miragenty 当前完成判定是"必须调 task_complete"，所以**两边的 hook 点不一样**：

- Claude Code：no-tool-use → check budget → 继续就 nudge / 停就退出
- Miragenty：task_complete + guardrail pass → 完成；否则按现有逻辑下一 step

**Miragenty 的 budget check 应该放在哪？** 我选 **每 step 末尾，task_complete 判定之前**：

```
[step N 末尾]
     │
     ├── 跑 token budget tracker → 决定本 step 后是 stop / continue
     │     │
     │     ├── continue → 不影响后续逻辑
     │     │
     │     └── stop → 注入一条 user message "你已用完 budget，请立即调 task_complete 总结当前进度"
     │                 然后 loop 继续，下一 step LLM 大概率会调 task_complete
     │
     └── 后续 task_complete / tool_use 处理同现状
```

为什么这样设计：不强行结束，而是**注入收尾提示让 LLM 自己调 task_complete**。原因：
- 强行 stop 会绕过 guardrail，artifact / commit 等收尾动作没机会做
- 让 LLM 自己 task_complete 能产出 summary，下游 evaluator/report 有素材
- 与 P0-1 / P0-3 的恢复语义保持一致——都是"注入提示让 agent 自己处理"，不是"强切"

---

## 3. 设计细节

### 3.1 BudgetTracker（新增 `agent/budget_tracker.rs`）

```rust
//! Token Budget tracker：基于"已花费 output tokens + 边际产出"的双信号停机器。
//!
//! 对标 Claude Code `query/tokenBudget.ts`。**只追踪 output_tokens**，不追踪 input：
//! - input tokens 主要受 history 长度驱动，不反映 agent 这轮 turn 的实际"产出"
//! - output tokens 直接对应 LLM "说了多少 / 调了多少 tool args"，是 agent 进展的真实信号
//!
//! 阈值（500 token / 3 轮）从 Claude Code 直接 port——他们在生产数据上调过，
//! 第一版照搬不调参，后续按 Miragenty 实测数据微调。

const DIMINISHING_TOKEN_THRESHOLD: u64 = 500;
const DIMINISHING_ROUND_THRESHOLD: u32 = 3;
const COMPLETION_PCT: f64 = 0.9;

#[derive(Debug, Clone)]
pub struct BudgetTracker {
    /// 当前已累计的 output tokens（跨 step 累加）
    accumulated_output_tokens: u64,
    /// 上一次 check 时的累计值，用于算 delta
    last_check_total: u64,
    /// 上一次 check 时算出的 delta
    last_delta: u64,
    /// 连续触发 continuation 的次数（用于 diminishing 判定）
    continuation_count: u32,
    /// 是否已经发过"该收尾了"的 nudge，避免每 step 都发
    nudge_emitted: bool,
}

impl BudgetTracker {
    pub fn new() -> Self {
        Self {
            accumulated_output_tokens: 0,
            last_check_total: 0,
            last_delta: 0,
            continuation_count: 0,
            nudge_emitted: false,
        }
    }

    /// 每 step 拿到 LLM response 后调用，记录本 step 的 output tokens。
    pub fn record_step(&mut self, step_output_tokens: u64) {
        self.accumulated_output_tokens += step_output_tokens;
    }

    /// 每 step 末尾决策。
    pub fn decide(&mut self, budget: u64) -> BudgetDecision {
        let total = self.accumulated_output_tokens;
        let delta = total.saturating_sub(self.last_check_total);
        let pct = if budget == 0 { 0.0 } else { (total as f64) / (budget as f64) };

        // diminishing：连续 N 轮都没新增多少 → agent 在原地打转
        let is_diminishing = self.continuation_count >= DIMINISHING_ROUND_THRESHOLD
            && delta < DIMINISHING_TOKEN_THRESHOLD
            && self.last_delta < DIMINISHING_TOKEN_THRESHOLD;

        self.last_delta = delta;
        self.last_check_total = total;

        if is_diminishing {
            return BudgetDecision::Stop {
                reason: BudgetStopReason::DiminishingReturns,
                accumulated: total,
                budget,
                pct,
            };
        }
        if total >= ((budget as f64) * COMPLETION_PCT) as u64 {
            return BudgetDecision::Stop {
                reason: BudgetStopReason::BudgetExhausted,
                accumulated: total,
                budget,
                pct,
            };
        }
        self.continuation_count += 1;
        BudgetDecision::Continue {
            accumulated: total,
            budget,
            pct,
            continuation_count: self.continuation_count,
        }
    }

    pub fn mark_nudge_emitted(&mut self) {
        self.nudge_emitted = true;
    }

    pub fn nudge_already_emitted(&self) -> bool {
        self.nudge_emitted
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BudgetStopReason {
    DiminishingReturns,
    BudgetExhausted,
}

#[derive(Debug, Clone)]
pub enum BudgetDecision {
    Continue {
        accumulated: u64,
        budget: u64,
        pct: f64,
        continuation_count: u32,
    },
    Stop {
        reason: BudgetStopReason,
        accumulated: u64,
        budget: u64,
        pct: f64,
    },
}
```

### 3.2 主循环集成（`engine.rs`）

#### 3.2.1 AgentRunOptions 加字段

```rust
pub struct AgentRunOptions {
    // ...
    /// 单 agent 整个 task 的 output token 软上限。None = 关闭（走 max_steps 旧逻辑）。
    /// 推荐值：模型 max_output_tokens × max_steps × 0.5（给 50% 余量做摘要 / 收尾）。
    /// e.g. claude-sonnet-4 max_output=8192, max_steps=80 → budget=327680
    pub output_token_budget: Option<u64>,
}
```

`AgentRunOptions::default()` 里设 `output_token_budget: None`，保持向后兼容。

#### 3.2.2 创建 tracker

```rust
// run_inner 函数开头
let mut budget_tracker = opts.output_token_budget.map(|_| BudgetTracker::new());
```

#### 3.2.3 每 step 累计

```rust
// 现 engine.rs:953 self.accumulate_agent_cost 之后
if let (Some(tracker), Some(_)) = (budget_tracker.as_mut(), opts.output_token_budget) {
    tracker.record_step(response.usage.output_tokens);
}
```

#### 3.2.4 每 step 末尾决策（关键点）

放在 `task_complete` 判定**之前**——给 budget 信号优先权，让"已经撞软上限"能影响下一 step 行为：

```rust
// 现 engine.rs:1080 task_complete 判定之前，对应 stop_reason / max_tokens 分支之后
if let (Some(tracker), Some(budget)) = (budget_tracker.as_mut(), opts.output_token_budget) {
    let decision = tracker.decide(budget);
    match decision {
        BudgetDecision::Stop { reason, accumulated, budget: b, pct } if !tracker.nudge_already_emitted() => {
            tracker.mark_nudge_emitted();
            let reason_str = match reason {
                BudgetStopReason::DiminishingReturns => "diminishing_returns",
                BudgetStopReason::BudgetExhausted => "budget_exhausted",
            };
            let nudge = format!(
                "[System] You have used {accumulated} output tokens out of your {b} budget ({:.0}%). \
                 Reason: {reason_str}. **Stop exploring** and call `task_complete` now with a summary of \
                 your progress. If the task isn't fully done, summarise what's done and what's not — \
                 do not start new work in this turn.",
                pct * 100.0
            );
            // 注入路径分两种（同 max_tokens hint）：
            //   - 本 step 有 tool_calls → 并入 follow-up
            //   - 本 step 无 tool_calls → 独立 user message
            // 这里在 task_complete 判定之前，tool_use_blocks 已经解析完成
            if tool_use_blocks.is_empty() {
                messages.push(Message {
                    role: MessageRole::User,
                    content: vec![ContentBlock::Text { text: nudge.clone() }],
                    cache_control: None,
                });
            } else {
                pending_max_tokens_hint = Some(
                    pending_max_tokens_hint.map(|prev| format!("{prev}\n\n{nudge}")).unwrap_or(nudge.clone())
                );
            }
            self.emit_event_with_meta(
                agent_id, step, "system_hint",
                &format!("Token budget {reason_str}: {accumulated}/{b} ({:.0}%); asked agent to wrap up", pct * 100.0),
                Some(serde_json::json!({
                    "kind": "budget_stop_nudge",
                    "reason": reason_str,
                    "accumulated_tokens": accumulated,
                    "budget": b,
                    "pct": pct,
                })),
            );
        }
        BudgetDecision::Continue { accumulated, pct, continuation_count, .. } => {
            tracing::debug!(
                agent_id = %agent_id, step, accumulated, pct = pct * 100.0, continuation_count,
                "token budget continue"
            );
        }
        _ => { /* already nudged */ }
    }
}
```

**注意：不强制 stop**——只是注入收尾提示。这样 task_complete + guardrail 流程完整保留。max_steps 是兜底硬上限。

### 3.3 ConfigManager 默认值

`AppConfig` 加字段 `agent_output_token_budget: Option<u64>`，默认按模型 capability 自动算（registry 已经有 `context_window`）：

```rust
fn default_output_token_budget(model: &str) -> u64 {
    // 估算：context_window × 30% 作为"agent 一个 task 累计 output" 预算
    // 30% 来自经验：output 通常是 input 的 1/3~1/5，task 完成时 output 占 context 的 30% 已是"高消耗"
    let cap = crate::llm::registry::get_capabilities(model);
    ((cap.context_window as f64) * 0.30) as u64
}
```

Settings UI 加一个"agent output token budget"输入（可关闭、可手动覆盖）。

---

## 4. 验收

### 4.1 单元测试（`budget_tracker.rs`）

```rust
#[test]
fn test_continue_when_under_budget_and_producing() {
    let mut t = BudgetTracker::new();
    t.record_step(2000);
    let d = t.decide(10_000);
    assert!(matches!(d, BudgetDecision::Continue { .. }));
}

#[test]
fn test_stop_when_above_90pct() {
    let mut t = BudgetTracker::new();
    t.record_step(9_500);
    let d = t.decide(10_000);
    assert!(matches!(d, BudgetDecision::Stop { reason: BudgetStopReason::BudgetExhausted, .. }));
}

#[test]
fn test_stop_on_diminishing_returns() {
    let mut t = BudgetTracker::new();
    // 3 轮 continuation 都 < 500 token delta
    for _ in 0..4 {
        t.record_step(200);
        let _ = t.decide(100_000);  // budget 富裕
    }
    t.record_step(200);
    let d = t.decide(100_000);
    assert!(matches!(d, BudgetDecision::Stop { reason: BudgetStopReason::DiminishingReturns, .. }));
}

#[test]
fn test_no_diminishing_when_one_big_round_mixed_in() {
    let mut t = BudgetTracker::new();
    t.record_step(100); let _ = t.decide(100_000);
    t.record_step(100); let _ = t.decide(100_000);
    t.record_step(5000); let _ = t.decide(100_000);  // 一轮大产出重置
    t.record_step(100);
    let d = t.decide(100_000);
    assert!(matches!(d, BudgetDecision::Continue { .. }));
}

#[test]
fn test_nudge_only_emitted_once() {
    let mut t = BudgetTracker::new();
    t.record_step(9_500);
    let _ = t.decide(10_000);
    t.mark_nudge_emitted();
    assert!(t.nudge_already_emitted());
    // 后续 decide 仍返回 Stop，但 caller 检查 nudge_already_emitted 跳过
}
```

### 4.2 集成测试

```rust
async fn test_budget_exhaustion_triggers_wrap_up_nudge() {
    // StubProvider 在每 step 都返回 ~1000 output tokens
    // budget=5000 → 第 5 step 后应触发 nudge
    // 期望：events 包含一条 system_hint with meta.kind="budget_stop_nudge"
    // 期望：第 6 step 的 messages 末尾包含 "Stop exploring and call task_complete"
}

async fn test_diminishing_returns_triggers_nudge_even_with_budget_left() {
    // StubProvider 连续 5 step 都返回 100 output tokens
    // budget=100000 → 预算根本没用满，但 diminishing 触发
}

async fn test_no_nudge_when_budget_disabled() {
    // opts.output_token_budget = None → 永远不 nudge，跑到 max_steps fail
}
```

### 4.3 手动 E2E

复现 calculator postmortem tester#2 场景：
- 现状：step 80 撞顶 fail，5.5M tokens 全废
- 改造后：step 50-60 之间触发 diminishing_returns nudge → agent 调 task_complete with partial summary → 整 agent 标 completed（partial）→ scheduler 走 evaluator → mission 进度推进而非 retry

---

## 5. 风险

| 风险 | 缓解 |
| --- | --- |
| 预算太小导致 agent 没真正完成就被催停 | 默认值按 context_window × 30% 自动算；用户可在 Settings 关闭/调大 |
| Diminishing 误判（agent 真的在做大改动只是输出短） | 阈值 500 token + 3 轮共识；可在 Settings 调阈值（暂不暴露，按需加） |
| 跟 max_steps 互相干扰 | 明确文档：budget 是软上限优先，max_steps 是硬上限兜底。budget 触发不影响 max_steps；max_steps 兜底逻辑保持现状 |
| task_complete 后 guardrail 失败仍会进 retry，retry 时 budget 已耗尽 | guardrail retry 不消耗新 budget（retry budget 自有 `guardrail_retry_budget`）。budget tracker 在 task_complete 后冻结 |

---

## 6. 落地清单

### 6.1 文件清单

| 文件 | 变更 | 行数 |
| --- | --- | --- |
| `src-tauri/src/agent/budget_tracker.rs` | **新建**：tracker + 5 个单测 | ~180 |
| `src-tauri/src/agent/mod.rs` | `mod budget_tracker; pub use budget_tracker::*;` | 2 |
| `src-tauri/src/agent/engine.rs` | (a) `AgentRunOptions::output_token_budget`; (b) tracker 创建; (c) `record_step`; (d) `decide` + nudge 注入; (e) 3 个集成测试 | ~140 |
| `src-tauri/src/commands/config.rs` | `AppConfig::agent_output_token_budget` + 默认值计算函数 | ~30 |
| `src/views/SettingsView.tsx` | Token Budget 输入（可关闭） + i18n 文案 | ~40 |
| `src/i18n/locales/{en-US,zh-CN}.json` | settings.token_budget.* 5 条 key | ~10 |

合计 ~400 行。

### 6.2 commit 计划

```
feat(agent): add output token budget tracker with diminishing returns

Long sessions used to fail at max_steps even when the agent was 80% done.
Now the engine maintains an output_token budget per agent; when either
(a) accumulated output >= 90% of budget, or (b) deltas drop below 500
tokens for 3 consecutive rounds (diminishing returns), the engine injects
a one-shot "wrap up via task_complete" nudge instead of letting the
agent burn the rest of max_steps doing nothing.

max_steps remains as a defensive hard ceiling. The budget is opt-in
(default: context_window * 30%).

Module: FM-02
```

```
test(agent): cover budget tracker and engine nudge injection
```

```
feat(commands,ui): expose agent output token budget in Settings
```

### 6.3 验证

- `cargo test --lib agent::budget_tracker` 5/5 绿
- `cargo test --lib agent::engine::tests::budget` 3/3 绿
- 跑 §4.3 E2E

---

## 7. 未来扩展

1. **Provider-aware budget**：现在按全局 context_window 算，未来按 reseller 实际限速 / 模型类型差异化
2. **Per-step nudge 文案模板化**：把 nudge 文本抽到 prompt template，方便用户自定义
3. **跟 P2-1 Stop Hook 配合**：让用户可以在 `BudgetDecision::Stop` 时跑自定义 hook（比如先 lint 再决定是否真停）
