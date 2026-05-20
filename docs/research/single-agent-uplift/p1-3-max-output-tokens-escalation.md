# P1-3：max_output_tokens 升级式重试 —— 撞顶先升档再求人拆分

> **目标**：把"`stop_reason == length`/`max_tokens` 撞顶 → 直接吐"请拆分"提示"改成**两阶段恢复**：① 同 step 直接把 max_tokens 升档到 64K 重试 → ② 仍撞顶才走 multi-turn "continue from where you cut off" 恢复 → ③ 多次失败才 surface 给用户。
>
> **对标**：Claude Code `query.ts:1264-1331` 的 `ESCALATED_MAX_TOKENS` 升级路径 + multi-turn recovery 循环。
> **本系列定位**：单 agent 鲁棒性升级 7 篇之 6。前置依赖：P0-3 (recovery_log)。

---

## 1. 现状

### 1.1 当前撞顶处理

`engine.rs:1023-1079`：

```rust
if response.stop_reason == "length" 
    || response.stop_reason == "max_tokens"
    || response.stop_reason == "max_output_tokens"
{
    let hint = format!(
        "[System] Your previous response hit the {} max_tokens output budget and \
         was cut off mid-response. ... \
         **Strategy**: Split large file content across multiple smaller tool calls. ...",
        opts.max_output_tokens
    );
    self.emit_event_with_meta(agent_id, step, "system_hint", ...);
    // 注入路径：有 tool_use → 并入 follow-up；无 tool_use → 独立 user msg
    // ...
}
```

**问题**：默认 `max_output_tokens = 16_384`（`DEFAULT_AGENT_MAX_OUTPUT_TOKENS`），一个稍微长的 diff（比如改 200 行代码 + ~5K 解释）就会撞顶。当前唯一应对是**催 LLM 拆**，LLM 重试时还可能继续撞——一次拆失败要 2-3 step。

### 1.2 Claude Code 的三档恢复

```typescript
// query.ts:1264-1331
if (isWithheldMaxOutputTokens(lastMessage)) {
    // ① 第一档：升档同一请求重试
    if (maxOutputTokensOverride === undefined && !envOverride) {
        state.maxOutputTokensOverride = ESCALATED_MAX_TOKENS;  // 64K
        continue;
    }
    
    // ② 第二档：multi-turn recovery
    if (maxOutputTokensRecoveryCount < MAX_OUTPUT_TOKENS_RECOVERY_LIMIT) {
        const recoveryMessage = createUserMessage({
            content: `Output token limit hit. Resume directly — no apology, no recap of what you were doing. 
                      Pick up mid-thought if that is where the cut happened. Break remaining work into smaller pieces.`,
            isMeta: true,
        });
        state.maxOutputTokensRecoveryCount++;
        // 注意：不 strip 前面 assistant message，让 LLM 接着写
        continue;
    }
    
    // ③ 第三档：surface
    yield lastMessage;
}
```

三档差异：
- **第一档（升档）**：相同 prompt，把 max_tokens 从 8K 升到 64K。**适用 80% 撞顶场景**（实际只是稍微超过 8K）
- **第二档（multi-turn）**：64K 还撞 → 让 LLM 接着写，分多 turn 完成
- **第三档（surface）**：3 次 multi-turn 还撞 → 真有问题，让用户介入

---

## 2. 目标行为

### 2.1 三档流转

```
[stop_reason ∈ {length, max_tokens}]
     │
     ├── per-step state.escalated_once? 
     │
     ├── ① 未升档 + 当前 max_tokens < ESCALATED_MAX_TOKENS (64K):
     │     │
     │     ├── emit recovery_attempt(MaxOutputTokens, OutputTokensEscalate{old, new=64K})
     │     ├── set escalated_once = true
     │     ├── overrides_max_tokens_this_step = 64K
     │     ├── strip 当前 step 的 assistant message（因为是要重发的）
     │     └── continue（同 step 重发同 messages 用新 max_tokens）
     │
     ├── ② 已升档 (64K 也撞) OR 原本就 >= 64K:
     │     │
     │     ├── 检查 multi_turn_recovery_count < 3
     │     │     │
     │     │     ├── 是：
     │     │     │   ├── emit recovery_attempt(OutputTokensContinue{n=count+1})
     │     │     │   ├── 注入 user message "Resume directly. No apology. Break into smaller pieces."
     │     │     │   ├── 保留 assistant message（让 LLM 接着写）
     │     │     │   ├── multi_turn_recovery_count++
     │     │     │   └── 进入下个 step（不 continue 同 step——新 turn）
     │     │     │
     │     │     └── 否（≥ 3）：
     │     │           ├── emit error（surface 真错误）
     │     │           ├── 注入现 §1.1 的 "请拆分" 提示作为最后一搏
     │     │           └── 继续（不 fail，但不再恢复）
     │
     └── 同步 step end，进入下 step
```

### 2.2 关键不变量

- **state 跨 step 还是 per-step？**
  - `escalated_once` 是**per-step**：每个 step 内只升档一次；下个新 step 又有 8K → 64K 的升档机会
  - `multi_turn_recovery_count` 是**整 agent 累计**：3 次硬上限不重置
  - 之所以这样分：升档是"同一思路重发"，每个 step 是独立思路所以可重新升；multi-turn recovery 是"agent 一直在写超长"，连续 3 次说明任务设计有问题

- **strip assistant message vs 保留**：
  - 升档路径：strip——同一思路重发，旧 partial 没用
  - multi-turn：保留——LLM 看着自己写到一半的内容接着写

---

## 3. 设计细节

### 3.1 常量与 state

```rust
// engine.rs 模块顶部
const ESCALATED_MAX_OUTPUT_TOKENS: u32 = 65_536;
const MAX_OUTPUT_TOKENS_RECOVERY_LIMIT: u32 = 3;
```

`run_inner` 新增 state：

```rust
let mut escalated_once_this_step = false;
let mut multi_turn_recovery_count: u32 = 0;
let mut current_max_output_tokens: u32 = opts.max_output_tokens;
```

### 3.2 主循环改造

#### 3.2.1 LLM request 用 current_max_output_tokens

```rust
let request = LlmRequest {
    // ...
    max_tokens: current_max_output_tokens,   // ← 不再用 opts.max_output_tokens
    // ...
};
```

#### 3.2.2 stop_reason 分支重写

替换现 `engine.rs:1023-1079` 整段：

```rust
let is_max_tokens_hit = matches!(
    response.stop_reason.as_str(),
    "length" | "max_tokens" | "max_output_tokens"
);

if is_max_tokens_hit {
    // ① 升档分支
    if !escalated_once_this_step && current_max_output_tokens < ESCALATED_MAX_OUTPUT_TOKENS {
        let old_cap = current_max_output_tokens;
        let new_cap = ESCALATED_MAX_OUTPUT_TOKENS;
        escalated_once_this_step = true;
        current_max_output_tokens = new_cap;

        self.emit_recovery_attempt(
            agent_id, step, RecoveryTrigger::MaxOutputTokens,
            RecoveryStrategy::OutputTokensEscalate { old_cap, new_cap },
            &format!("stop_reason={}", response.stop_reason), 1,
        );

        // strip 当前刚收到的 assistant message——升档相当于重发同一 prompt
        // 注意：上面已经 messages.push 了这条 assistant，需要 pop 掉
        if let Some(last) = messages.last() {
            if matches!(last.role, MessageRole::Assistant) {
                messages.pop();
            }
        }
        // 同时回退 cost 累计 —— pop 掉的 assistant 不算（已写库的 step_cost / cost_record
        // 是另一条路径，业务上视为"重试成本"保留即可，不回滚）
        continue;
    }

    // ② multi-turn recovery 分支
    if multi_turn_recovery_count < MAX_OUTPUT_TOKENS_RECOVERY_LIMIT {
        multi_turn_recovery_count += 1;
        self.emit_recovery_attempt(
            agent_id, step, RecoveryTrigger::MaxOutputTokens,
            RecoveryStrategy::OutputTokensContinue { recovery_count: multi_turn_recovery_count },
            &format!("stop_reason={}, recovery #{}", response.stop_reason, multi_turn_recovery_count),
            multi_turn_recovery_count,
        );

        let recovery_msg = "[System] Output token limit hit. Resume directly — no apology, no recap of what you were doing. Pick up mid-thought if that is where the cut happened. Break remaining work into smaller pieces (separate tool calls / shorter writes).";
        // 注入到 messages：保留 assistant message（让 LLM 接着写）
        // 由于是新 turn，独立 user message 合规（前一个 assistant 是 text-only/half-truncated，不带 valid tool_calls）
        // 但保险起见沿用 follow-up builder 模式时要看 tool_use_blocks 解析后再注入
        pending_max_tokens_hint = Some(recovery_msg.to_string());
        // 注意：不 continue，让正常 follow-up 流程把 hint 拼到 message 里
        // current_max_output_tokens 保持 ESCALATED 不回退
    } else {
        // ③ 真撞墙了，surface
        let surface_msg = format!(
            "[Error] Hit max_output_tokens 4 times in a row (escalated to {ESCALATED_MAX_OUTPUT_TOKENS} tokens, then {MAX_OUTPUT_TOKENS_RECOVERY_LIMIT} multi-turn recoveries). Task is too large for a single turn — split into smaller chunks via separate tool calls."
        );
        self.emit_event_with_meta(
            agent_id, step, "error", &surface_msg,
            Some(serde_json::json!({
                "kind": "max_output_tokens_exhausted",
                "escalated_to": ESCALATED_MAX_OUTPUT_TOKENS,
                "recovery_attempts": multi_turn_recovery_count,
            })),
        );
        // 不直接 fail——继续走 follow-up，让 LLM 看到上面 surface_msg 再决定
        pending_max_tokens_hint = Some(surface_msg);
    }
}
```

#### 3.2.3 step 边界重置

```rust
// loop 顶部，跟 attempted_reactive_compact_this_step 同位置
escalated_once_this_step = false;
// 注意：multi_turn_recovery_count 不重置（跨 step 累计）
// current_max_output_tokens 也不重置（升档后保持升档值，下 step 也能用大窗口）
```

**为什么 escalated_once 重置但 current_max_output_tokens 不重置**：
- escalated_once 是"本 step 内是否已经升档" → 下 step 新一轮可以再升一次（如果下 step 又降回 8K 而撞顶）。但实际不会，因为我们不降回。
- current_max_output_tokens 是"当前使用的 cap" → 升上去了就不降，下 step 直接用 64K，减少撞顶概率

更准确地说：**current_max_output_tokens 是单调上升的**（直到 agent 结束）。这给一个潜在的优化空间：下次同 agent 直接 default 64K 即可，但暂不做（保持初始 16K 防爆成本）。

### 3.3 与现有 read-only-loop / max-tokens hint 注入的协调

`engine.rs:1207-1222` 现有 `pending_read_only_hint` 和 `pending_max_tokens_hint` 在 follow-up 阶段一起注入。改造后：

- `pending_max_tokens_hint` 在 ② / ③ 分支被 set
- ① 升档分支不进 follow-up（直接 continue 重发），所以不影响

### 3.4 Recovery 节流（防滥用）

如果 LLM 反复在多个 step 内升档 + multi-turn，累计 cost 会失控。加一个 mission 级 cap：

```rust
// mission 配置：max_recovery_attempts_per_mission (默认 20)
// 在 emit_recovery_attempt 时检查，触达上限则 surface error + 标 task 为 needs_review
```

本 PR 暂不实现，留 P2 与 P0-3 配套做。

---

## 4. 验收

### 4.1 单元测试

集中在主循环行为，用 mock provider：

```rust
async fn test_escalates_max_tokens_on_first_hit() {
    // StubProvider step 1 返回 stop_reason="length" with max_tokens=16K
    // 期望：
    //   - events 含 recovery_attempt with strategy.kind="output_tokens_escalate", old=16K, new=64K
    //   - 第二次 LLM 调用的 max_tokens=64K（通过 mock 监控）
    //   - messages 末尾不含上一次 partial assistant
}

async fn test_escalation_skipped_when_already_at_64k() {
    // opts.max_output_tokens=65536 起步 → 撞顶直接走 multi-turn 分支
}

async fn test_multi_turn_recovery_runs_up_to_limit() {
    // StubProvider 连续 4 step 都返回 stop_reason=length
    // 期望：
    //   - 第 1 step 升档
    //   - 第 2-4 step multi-turn recovery（count 1,2,3）
    //   - 第 5 step surface error（不再恢复）
}

async fn test_per_step_escalation_resets() {
    // step 1 升档 → step 1 task 完成
    // step 2 又撞顶（current_max_output_tokens 已是 64K）→ 直接走 multi-turn
    // 断言：step 2 仍然能触发 multi-turn（escalated_once_this_step 重置）
}

async fn test_current_max_output_tokens_persists_across_steps() {
    // step 1 升档到 64K → step 2 LLM 调用 max_tokens 应是 64K
}
```

### 4.2 手动 E2E

写一个会触发的 prompt："帮我写一个 100 行的 Rust 函数 + 详细注释 + 单测"：

- 旧行为：撞顶 → 看到"请拆分"hint → LLM 重试拆 → 平均 3-4 step 才完成
- 新行为：第一次撞顶静默升档 64K → 一次过完成

---

## 5. 风险

| 风险 | 缓解 |
| --- | --- |
| 64K 输出导致 cost 飙升 | 仅在撞顶后才升档；正常 step 还是 8K |
| Reseller 不支持 max_tokens=65536（如某些 32K context 模型） | 在升档前 clamp 到 `model_capability.context_window / 2`；详见 §3.5 |
| Multi-turn recovery 让 LLM 跑成"无限续写" | 3 次硬上限 + ③ surface error 不再恢复 |
| strip assistant message 在 streaming tool execution (P1-1) 下混乱 | 与 P1-1 集成：升档分支同时 `executor.abort_all()` 清空 |

### 3.5 升档值 clamp

```rust
fn compute_escalated_max_tokens(model: &str) -> u32 {
    let cap = crate::llm::registry::get_capabilities(model);
    // 不超过模型 context_window 的一半，且不超过 64K
    let upper = (cap.context_window / 2) as u32;
    upper.min(ESCALATED_MAX_OUTPUT_TOKENS).max(opts.max_output_tokens)
}
```

如果模型 context_window=32K（如 deepseek-coder-32k），升档值会被 clamp 到 16K—结果可能等于原值，等于跳过 ① 直接走 ②。这是 OK 的：对这类小窗口模型，escalation 本身没意义。

---

## 6. 落地清单

| 文件 | 变更 | 行数 |
| --- | --- | --- |
| `src-tauri/src/agent/engine.rs` | (a) 常量; (b) state 字段; (c) 三档分支; (d) clamp 计算 | ~120 |
| `src-tauri/src/agent/engine.rs` 测试 | 5 个集成测试 | ~250 |
| `src-tauri/src/agent/recovery_log.rs` | RecoveryStrategy 加 OutputTokensEscalate / OutputTokensContinue 变体（部分在 P0-3 已规划）| ~20 |

合计 ~390 行。

### commit 计划

```
feat(agent): escalate max_output_tokens before falling back to multi-turn

When stop_reason indicates output truncation, the engine now first
re-issues the same request with max_output_tokens raised to 64K (clamped
by model context window). Only if 64K also truncates does it fall back
to the multi-turn "resume directly" recovery loop, which itself caps at
3 attempts before surfacing the error.

This eliminates the common "one large diff truncated → forced split →
re-truncated → another split" loop that wasted 3-4 steps per occurrence.

Module: FM-02
```

```
test(agent): cover max_output_tokens escalation and multi-turn recovery
```

---

## 7. 与其它优化项的关系

- **P0-3 (recovery_log)**：本 PR 的三档恢复都走 `emit_recovery_attempt`；其中 ① 升档完全静默（silent=true），② 静默但用户能在 dev mode 看到，③ surface（非 silent）
- **P1-1 (Streaming Tool Execution)**：升档分支需要 `executor.abort_all()` 清掉已 spawn 的 partial tools
- **P1-2 (Cross-model Fallback)**：fallback 和 max_tokens 不冲突。如果 fallback model 也撞 max_tokens → 走本 PR 三档流程
