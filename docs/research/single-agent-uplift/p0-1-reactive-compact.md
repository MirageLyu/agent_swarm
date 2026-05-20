# P0-1：Reactive Compact —— API 真返回 413 / context_length_exceeded 后的二次救援

> **目标**：让单 agent 在 LLM 端真的拒掉一次"prompt 太长"的请求时，**先尝试一次压缩 + 重发同一 step**，而不是直接把 step 失败丢给 scheduler。
>
> **对标**：Claude Code 的 `services/compact/reactiveCompact.ts` + `query.ts:1158-1259`。
> **本系列定位**：单 agent 鲁棒性升级 7 篇之 1。剩余 6 篇见同目录其他文件。

---

## 1. 现状（撞墙处）

### 1.1 触发场景

Agent 主循环 `src-tauri/src/agent/engine.rs:860-928` 把 stream 错误分两类：

| 错误源 | 现处理 | 用户感知 |
| --- | --- | --- |
| `StreamGuardError::IdleTimeout` | 进 `idle_retries_left` 重试，注入 "[System] 上一次响应中断" continue 提示 | OK，体感"卡了几下，自己续上了" |
| `StreamGuardError::Llm(msg)` | 直接 `return Err(anyhow!(e.user_message_zh()))` → step fail | **致命**：context 超长这种**确定性可恢复**故障当不可恢复故障吃 |

`openai_compat::stream_chat`（`llm/openai_compat.rs:411-413`）在 HTTP 400/413 时直接 `bail!("OpenAI compat API error {status}: {text}")`，错误文本含 `"context_length_exceeded"` / `"prompt is too long"` / `"This model's maximum context length is X tokens"` 等关键词。当前没有任何代码识别这些关键词。

### 1.2 现有 microcompact 为什么救不了

`engine.rs:285` 的 `microcompact()` 在**主循环开头**按 50K token 估算阈值预防性触发。问题：

1. **粗估失真**：`approximate_tokens = chars / 4` 对 reasoning content / JSON-heavy tool args 经常低估 30-50%，等实际撞到 API 限制时，本地估算可能才 60K。
2. **触发不了**：50K 是个固定常数，但用户配 `claude-sonnet-4` (200K ctx) 跑长任务时，50K 触发等于太早；用 `qwen-32k` (32K ctx) 跑时，50K 永远不会触发。
3. **无 retry 语义**：microcompact 触发后只是让下一轮 step 用更短的 messages，对**当前**已经失败的 step 没有补救。

### 1.3 撞墙的真实代价

按 `2026-05-18-macos-calculator-postmortem.md` 数据：一个 6-step agent 平均 ~30s/step，撞墙后整 agent failed → 走 `restart_mission(failed_only)` → cold-start 重跑前 5 个 step。**这次代价 ≈ 5 step × 30s × 模型成本 ≈ 整个 agent 一遍**。

---

## 2. 目标行为（合同）

```
[step N] LLM 调用
     │
     ├── 成功 → 正常往下走 (tool_use / task_complete)
     │
     ├── StreamGuardError::Llm 但**不是** context 超长 → 维持现状，bail
     │
     └── StreamGuardError::Llm 且识别为 context 超长 → 进 Reactive Compact 分支：
            │
            ├── 已经在本 step 内 reactive-compact 过一次 → bail（防死循环）
            │
            └── 否则：
                  ├── emit `compact` 事件 (kind: reactive, trigger: prompt_too_long)
                  ├── 强制运行一次 microcompact（drop_count 提升到 1/2，更激进）
                  ├── 标记 `attempted_reactive_compact_this_step = true`
                  ├── 当前 step **不消费 step 号**（loop continue 时 step -= 1 已经在 step += 1 之前）
                  └── 重发同一 LLM 请求
```

**关键不变量**：

- **每个 step 最多一次 reactive compact**（用 `attempted_reactive_compact_this_step` flag 守住）
- **不跨 step 累计**（下个 step 重置 flag）→ 长 session 里每个 step 都有机会做一次自救
- **provider 错误识别只看错误消息**，不强依赖 HTTP status（reseller 经常把 413 包成 200+ body）
- **不替换现有 microcompact / proactive 路径**，是兜底层

---

## 3. 设计细节

### 3.1 错误分类器（新增 `llm/error_class.rs`）

```rust
//! Stream / chat 错误的轻量分类器：识别"哪些错误值得在 agent 主循环里做差异化处理"。
//!
//! 设计原则：
//! - 只看错误**消息**，不看 HTTP status（reseller 行为不一致，body 里 "context length" 的
//!   误报率远低于 status code）
//! - 关键词集合**故意保守**：宁可漏判为 Generic 让它真 fail，也别误判把网络抖动当 context
//!   超长触发 compact（compact 自己也耗 token）
//! - 分类纯函数，便于单测覆盖各家 provider 的真实错误消息

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlmErrorClass {
    /// Anthropic: "prompt is too long: ... tokens > X maximum"
    /// OpenAI:    "This model's maximum context length is X tokens..."
    /// DeepSeek:  "context_length_exceeded" (in error.code)
    /// 通义:      "Range of input length should be ... InputTokensLimit"
    PromptTooLong,
    /// 暂未细分的所有其它错误。
    Generic,
}

pub fn classify_llm_error(msg: &str) -> LlmErrorClass {
    let lower = msg.to_lowercase();
    const PROMPT_TOO_LONG_NEEDLES: &[&str] = &[
        "context_length_exceeded",     // OpenAI / DeepSeek error code
        "prompt is too long",           // Anthropic error string
        "maximum context length",       // OpenAI error string
        "input tokens limit",           // 通义/DashScope
        "string too long",              // 少见但出现过
        "prompt too long",              // 同义
        "context length",               // 通用兜底
        "max_input_tokens",             // 某些代理转译
    ];
    for needle in PROMPT_TOO_LONG_NEEDLES {
        if lower.contains(needle) {
            return LlmErrorClass::PromptTooLong;
        }
    }
    LlmErrorClass::Generic
}
```

测试矩阵（单测覆盖）：

| Provider | 真实错误消息样本 | 期望分类 |
| --- | --- | --- |
| OpenAI | `"This model's maximum context length is 128000 tokens. However, you requested 195000 tokens"` | PromptTooLong |
| Anthropic | `"prompt is too long: 234234 tokens > 200000 maximum"` | PromptTooLong |
| DeepSeek | `"OpenAI compat API error 400: {\"error\":{\"code\":\"context_length_exceeded\",...}}"` | PromptTooLong |
| 通义 | `"Range of input length should be [1, 30720]. InputTokensLimit: 30720"` | PromptTooLong |
| 网络错误 | `"网络连接中断，请检查网络后重试"` | Generic |
| 鉴权错误 | `"OpenAI compat API error 401: invalid api key"` | Generic |
| 限速 | `"rate_limit_exceeded"` | Generic |

### 3.2 主循环改造点（`engine.rs`）

#### 3.2.1 状态新增

```rust
// run_inner 函数开头，跟 hinted_remaining_steps 同级别
let mut attempted_reactive_compact_this_step = false;
```

#### 3.2.2 错误分支处理

```rust
// 现 engine.rs:925 附近 stream_outcome 的 match 分支
Err(StreamGuardError::Llm(msg)) => {
    use crate::llm::error_class::{classify_llm_error, LlmErrorClass};
    if classify_llm_error(&msg) == LlmErrorClass::PromptTooLong
        && !attempted_reactive_compact_this_step
    {
        attempted_reactive_compact_this_step = true;
        // 强制一次激进 compact —— drop 一半而非 1/3，因为已经撞墙
        let report = reactive_compact_aggressive(&mut messages);
        let meta = serde_json::json!({
            "kind": "reactive",
            "trigger": "prompt_too_long",
            "error_excerpt": msg.chars().take(200).collect::<String>(),
            "report": report.as_ref().map(|r| r.to_meta()),
        });
        let human = match &report {
            Some(r) => format!(
                "API rejected request as too long; reactive compact dropped {} message(s) (~{}K → ~{}K tokens). Retrying same step.",
                r.dropped_messages, r.tokens_before / 1000, r.tokens_after / 1000,
            ),
            None => "API rejected request as too long but messages too few to compact further. Failing step.".to_string(),
        };
        self.emit_event_with_meta(agent_id, step, "compact", &human, Some(meta));
        if report.is_none() {
            // 没法再压了，认输
            return Err(anyhow::anyhow!(msg));
        }
        // 同 idle retry 一样：不让 loop 顶部重置 step；本 step 重发请求
        // **但 step 号已经在前面 step += 1 过了**——为保持 step 号语义稳定（前端 timeline
        // 一致性），这里**不**回退 step。重发后还是同一 step 号，只是 messages 变短了。
        continue;
    }
    return Err(anyhow::anyhow!(msg));
}
```

#### 3.2.3 Per-step flag 重置

```rust
// 在 loop 顶部，跟 idle_retries_left = next_idle_retry_budget(...) 同位置
attempted_reactive_compact_this_step = false;
```

### 3.3 Aggressive Compact 函数

```rust
/// reactive 版的 microcompact：撞墙后调用，比 proactive 版更激进。
///
/// 与 `microcompact()` 的差别：
///   - drop_count 从 `len()/3` 提升到 `len()/2`
///   - 没有 `len() >= 8` 的最小消息数门槛（4 起就压）
///   - summary 文本明确说明"由 API 拒绝触发"，让 LLM 知道前情
///
/// 返回 None 表示无能为力（messages < 4 或 drop 后剩余太少）。
fn reactive_compact_aggressive(messages: &mut Vec<Message>) -> Option<CompactReport> {
    // 至少留 2 条最新 messages 给 LLM 做上下文。messages < 4 时压完只剩 1 条
    // assistant 或 1 条 user，meaningless。
    if messages.len() < 4 {
        return None;
    }
    let before = approximate_tokens(messages);
    let drop_count = messages.len() / 2;
    let dropped: Vec<Message> = messages.drain(0..drop_count).collect();

    let mut tools_seen: Vec<String> = Vec::new();
    for msg in &dropped {
        for block in &msg.content {
            if let ContentBlock::ToolUse { name, .. } = block {
                if !tools_seen.contains(name) {
                    tools_seen.push(name.clone());
                }
            }
        }
    }

    let summary = format!(
        "[context-compact:reactive] The LLM API rejected the previous request as too long. \
         {} earlier message(s) have been aggressively compacted to free space and the request \
         is being retried. The full event history remains in the workspace timeline. \
         Tools you ran earlier: {}. Continue from the latest messages below.",
        drop_count,
        if tools_seen.is_empty() { "(none)".to_string() } else { tools_seen.join(", ") },
    );

    messages.insert(0, Message {
        role: MessageRole::User,
        content: vec![ContentBlock::Text { text: summary }],
        cache_control: None,
    });

    let after = approximate_tokens(messages);
    Some(CompactReport {
        dropped_messages: drop_count,
        tools_seen,
        tokens_before: before,
        tokens_after: after,
    })
}
```

### 3.4 事件 schema 扩展

`agent_events.kind` 现有 `compact` 值已能复用。**新增 meta 字段**（向后兼容，前端按需读）：

```jsonc
{
  "kind": "reactive",                  // proactive (现有，默认) | reactive (新增)
  "trigger": "prompt_too_long",        // 仅 reactive 时存在
  "error_excerpt": "...",              // 触发的原始错误消息前 200 字符
  "report": {                          // 同 proactive 的 to_meta()
    "dropped_messages": 12,
    "tokens_before": 89456,
    "tokens_after": 41123,
    "tools_seen": ["read_file", "edit_file"]
  }
}
```

**proactive 一侧也要补 `kind: "proactive"`** —— 现 `microcompact` 路径的 meta 没这个字段，加上后前端能在 timeline 上区分两种 compact 来源。

### 3.5 前端展示（最小改动）

`src/stores/agent-store.ts` 的 `AgentEvent` 类型已支持 `meta: Record<string, unknown>`，无需改类型。Workspace timeline 渲染 `compact` 事件时，若 `meta.kind === "reactive"`，加一个红色"⚠ Reactive"徽章；否则现状（蓝色）。

不在本 PR scope：reactive compact 历史的 Insights 聚合视图（后续 P1 再做）。

---

## 4. 验收

### 4.1 单元测试

文件 `src-tauri/src/llm/error_class.rs`：

- `test_classify_openai_context_too_long`
- `test_classify_anthropic_prompt_too_long`
- `test_classify_deepseek_context_length_exceeded`
- `test_classify_dashscope_input_tokens_limit`
- `test_classify_network_error_is_generic`
- `test_classify_auth_error_is_generic`
- `test_classify_rate_limit_is_generic`（防止误判，因为 rate_limit 是 P1-2 的 fallback 触发条件）

文件 `src-tauri/src/agent/engine.rs` 的 `#[cfg(test)] mod tests`：

- `test_reactive_compact_aggressive_drops_half`
- `test_reactive_compact_aggressive_returns_none_when_too_few_messages`
- `test_reactive_compact_summary_marks_reactive_origin`

### 4.2 集成测试（mock provider）

新增 `engine.rs` 测试模块（如已有，追加）：

```rust
/// 给 StubProvider 一个"第 N 次调用时报 context_length_exceeded"的能力，
/// 验证 agent 主循环能识别并触发 reactive compact 后续走通。
async fn test_reactive_compact_retries_same_step_on_prompt_too_long() {
    // 1. 构造 5-step plan：step 3 时 provider 报 context_length_exceeded
    // 2. 跑 agent
    // 3. 断言：
    //    - step 3 上 events 包含一条 kind=compact / meta.kind=reactive
    //    - step 3 上 events 包含两条 llm_call（同一 step 重发）
    //    - 最终 agent_status = completed（没因为单 step 失败而整 agent fail）
}

async fn test_reactive_compact_only_once_per_step() {
    // provider 在 step 3 连续两次报 context_length_exceeded
    // → 第二次必须真 fail，不能死循环
    // 断言：第二次错误 surface 出去 + agent_status = failed
}
```

### 4.3 手动 E2E（postmortem 复现）

按 `2026-05-18-macos-calculator-postmortem.md` §3.4 的 tester#2 prompt 重跑：

- 预期：原 case 中 stream 在 step ~60 撞 context 上限失败（200K tokens）
- 改造后：触发 reactive compact，messages 砍半，retry 同 step，成功继续到 task_complete
- 通过条件：完整 mission 跑完，期间出现至少 1 条 `meta.kind=reactive` 的 compact 事件

---

## 5. 风险与回滚

### 5.1 风险点

| 风险 | 触发条件 | 缓解 |
| --- | --- | --- |
| 误判（把限速错误当 context 超长） | 关键词集合扩太广 | 单测覆盖各家真实错误消息；新增关键词必须 PR review |
| Compact 后 messages 太短导致 LLM 失去任务上下文 | task description 本身在被 drop 的范围里 | 不动 system prompt（system 在 `LlmRequest::system`，不在 messages 内） |
| 死循环：reactive compact 后仍超长（system + 2 条最新已超） | system prompt 本身就 > 限额 | `attempted_reactive_compact_this_step` flag 保证单 step 只跑一次；第二次真 fail |
| Compact 干扰 prompt caching | drop 早期 messages 后 cache prefix 失效 | 这是 acceptable cost——撞墙的代价远高于一次 cache miss |
| 前端 timeline 渲染未更新导致 reactive 事件视觉同 proactive | UI 改动晚于后端 | 后端 meta 字段向后兼容，旧 UI 仍能正常渲染 |

### 5.2 回滚路径

- 单 commit 实现（见 §6），revert 即可
- 配置开关：`AgentRunOptions` 加 `enable_reactive_compact: bool`，默认 true；用户在 Settings 关掉等于退回旧行为
- 错误分类器即使关掉 reactive，本身也是 standalone 模块（P0-3 / P1-2 都会用），不会因关 reactive 而被打扰

---

## 6. 落地清单

### 6.1 改动文件清单

| 文件 | 变更 | 估计行数 |
| --- | --- | --- |
| `src-tauri/src/llm/error_class.rs` | **新建**，分类器 + 7 个单测 | ~120 |
| `src-tauri/src/llm/mod.rs` | 加 `mod error_class; pub use error_class::*;` | 2 |
| `src-tauri/src/agent/engine.rs` | (a) `reactive_compact_aggressive()` 新函数; (b) `attempted_reactive_compact_this_step` 状态; (c) `Err(StreamGuardError::Llm)` 分支识别; (d) 现有 `microcompact` 路径补 `kind: "proactive"` meta; (e) 3 个单测 | ~150 |
| `src-tauri/src/agent/engine.rs` 测试模块 | 2 个集成测试用 StubProvider 复现 | ~120 |

合计 ~390 行（其中代码 ~270 / 测试 ~120）。

### 6.2 commit 计划

按 commit-convention.mdc 拆 2 个 commit：

```
feat(llm): add reactive compact for prompt-too-long recovery

Recognise context-length-exceeded errors from Anthropic / OpenAI /
DeepSeek / DashScope responses, then trigger an aggressive in-step
compact (drop half of messages) and retry the same step instead of
failing the whole agent.

Each step is guarded by attempted_reactive_compact_this_step so
unrecoverable cases (system prompt itself too long) still bail.

Module: FM-02
```

```
test(llm): cover llm error classifier and reactive compact retry path
```

### 6.3 验证手段

- `cargo test --lib llm::error_class` 全绿
- `cargo test --lib agent::engine::tests::reactive_compact`  全绿
- `cargo test --lib` 全量（345+ 测试无回归）
- 跑 §4.3 的手动 E2E（可选，但强烈建议在 merge 前过一次）

---

## 7. 未来扩展（不在本 PR）

1. **Compact 触发的"二级摘要"**：reactive compact 触发后下次 step 重新 hit 阈值时，对已 compact 过的 summary 再 summary（套娃压缩）。
2. **触发节流**：单 mission 内 reactive compact > N 次时，弹 approval 让用户决定要不要继续（可能是任务真的太大）。
3. **Reactive Snip**：识别能被 snip 的旧 tool_result 块（如已被覆盖的旧 read），优先 snip 而非全量 compact。这条对接 P0-3 (per-tool budget) 后再做。
