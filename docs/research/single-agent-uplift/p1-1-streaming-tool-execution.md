# P1-1：Streaming Tool Execution —— 工具 use block 一边收一边并行开跑

> **目标**：把"等 LLM 全部响应完 → 解析全部 tool_use → 并行执行"改成"流式收到一个 tool_use block 就立刻调度执行"，让 tool 执行时间与 LLM 流式输出时间**重叠**。
>
> **对标**：Claude Code `services/tools/StreamingToolExecutor.ts` + `query.ts:914-935`。
> **本系列定位**：单 agent 鲁棒性升级 7 篇之 4（性能 / UX 类）。

---

## 实施状态（2026-05-20 更新）

P1-1 是 7 篇里**最重**的一条（~670 行跨越 llm/agent/UI 三层），分两阶段落地：

### Phase A（已完成 ✅）—— 协议升级 + Executor 状态机 + 生产者侧 emit

落地内容：
1. **`llm/types.rs`**：`StreamChunkKind::ToolUseStart` / `ToolUseInputDelta` 升级为带 `tool_use_id` 的 struct variant，新增 `ToolUseStop { tool_use_id }`
2. **`llm/openai_compat.rs`**：`stream_chat` 在累积 `tool_calls` 时同步 emit 三种新 chunk
   - 首次见到 id+name → `ToolUseStart`
   - 每次 args delta → `ToolUseInputDelta`（仅 id 已知时 emit）
   - finish_reason 后 → 给每个非空 id emit `ToolUseStop`
3. **`agent/streaming_tool_executor.rs`**（新文件）：`StreamingToolExecutor` 状态机 + `ToolDispatcher` trait + 9 个单测覆盖
   - safe 工具：on_stop 时立即 `tokio::spawn`，inflight 累积
   - unsafe 工具：on_stop 时进 queue，drain 阶段串行
   - `drain_in_order` 按 caller 指定顺序拿结果（API 协议要求）
   - `abort_all` 用 `JoinHandle::abort` 切断所有 inflight
4. **可见性调整**：`openai_compat::parse_tool_arguments_or_sentinel` 提升 `pub(crate)` 供 executor 复用 sentinel 协议

测试结果：515 全过（含 9 个新增 executor 单测）。0 行为变化（engine.rs 主循环**还未**接线新 executor，所有 tool 仍走老路径）。

### Phase B（独立 PR，待落地）—— Engine 接线

剩余工作：
1. `engine.rs` forwarder（~700 行处）识别 ToolUse* chunk → 喂 executor
2. step 末尾改 drain 逻辑：从「按 safe_flags 分桶 + 并发跑」改成「`executor.drain_in_order(ordered_ids)`」
3. cancel 联动：cancel handler 调 `executor.abort_all()`
4. 集成测试：StubProvider 模拟分块 emit，断言 tool 与 LLM 真正 overlap
5. 前端 `tool_use_started` 占位事件渲染

为什么拆：engine.rs 主循环 3000+ 行，接线改动会触碰 ~150 行核心调度路径，是 P0-1/P0-2/P0-3/P1-3 之外**首次**触碰这一段。隔离 Phase B 让 Phase A 协议变更独立 ready / rollback。

### Phase C（未来 nice-to-have）—— Anthropic Provider 支持

`anthropic.rs` 当前 stream 只处理 Text content_block，tool_use 还没支持。等团队真正接 Anthropic 直通模型时一并补。

---

---

## 1. 现状

### 1.1 当前执行链路（串行）

```
[t=0]   step 开始
[t=0]   发起 LLM stream 请求
[t=0+]  LLM 开始流式返回 chunks
[t=5s]  LLM 全部返回（含 3 个 tool_use block）
[t=5s]  解析 tool_use_blocks: Vec<(id, name, input)>
[t=5s]  按 safe/unsafe 分桶
[t=5s]  并行跑 safe 桶（read/search/list）
[t=5.3s] 3 个 read 全部完成（200ms × 3 ≈ 350ms 实际由网络/磁盘决定）
[t=5.3s] 拼 follow-up message → 进下一 step
```

总耗时 = **LLM 流式时间 + tool 执行时间**。一个典型的 "读 5 个文件再说思路" turn：5s LLM + 350ms reads = 5.35s。

### 1.2 Claude Code 的流式调度

```typescript
// query.ts:909-935
for await (const message of deps.callModel({...})) {
  if (message.type === 'assistant') {
    const msgToolUseBlocks = message.message.content.filter(c => c.type === 'tool_use')
    for (const toolBlock of msgToolUseBlocks) {
      streamingToolExecutor.addTool(toolBlock, message)  // ← 立即开跑
    }
  }
  // 边收边消费已完成的 tool result
  for (const result of streamingToolExecutor.getCompletedResults()) {
    yield result.message
  }
}
// stream 完成后：消费剩余 in-flight tools
for await (const update of streamingToolExecutor.getRemainingResults()) {
  yield update.message
}
```

同样 case：
```
[t=0]    step 开始 + LLM 流
[t=2s]   LLM 流到第一个 tool_use（read A）→ executor.addTool → tokio::spawn read A
[t=2.2s] read A 完成
[t=3s]   LLM 流到第二个 tool_use（read B）→ spawn read B
[t=3.2s] read B 完成
[t=4s]   LLM 流到第三个 tool_use（read C）→ spawn read C
[t=4.2s] read C 完成
[t=5s]   LLM 流结束，所有 results 已就绪
[t=5s]   拼 follow-up
```

总耗时 ≈ max(LLM 时间, 最慢 tool 完成时间) ≈ 5s。**节省 350ms (~6%)**。

### 1.3 真正的稳定性增益（不只是性能）

| 维度 | 串行 (现状) | 流式 (改造后) |
| --- | --- | --- |
| 短 read 多并发 (3-5 个) | +200~400ms | 0~50ms 增量 |
| 单 shell_exec 慢工具 (30s) | 用户等 LLM 5s + tool 30s = 35s 才看到下一步 | LLM 5s 内已 emit tool_use 事件 + tool 30s 在跑，用户看到"工具执行中"状态 |
| Cancel 时机 | 必须等 LLM 流完成 + tool 完成后才能 break | tool 执行中即可 cancel，P99 延迟降 |
| 异常 tool 干扰其它 tool | 串行下后续 tool 必须等异常 tool 超时（5min） | 异常 tool 单独 spawn，其它 tool 不受影响 |
| 用户体感"agent 卡住了吗" | LLM 5s + 工具 5s 整段没事件 | LLM stream 内即时 emit tool_use start，体感连续 |

最后一行是关键——**用户感知层面**才是真正的稳定性收益。当前架构下用户看到 "LLM 思考中..." 然后突然 "工具运行中..."，中间有 5 秒空窗；改造后 user 体验是连续的进度流。

---

## 2. 目标行为

### 2.1 合同

```
[step N 开始]
     │
     ├── 启动 LLM stream
     │
     └── 流式接收 chunks 同时：
            │
            ├── chunk = TextDelta / ReasoningDelta → 透传给前端（现状）
            │
            ├── chunk = ToolUseStart { id, name } → 记下"待启动" tool_use
            │
            ├── chunk = ToolUseInputDelta → 累积该 tool 的 input JSON
            │
            └── tool_use 的 input 完整（accumulated input 是 valid JSON） →
                  │
                  ├── 是 safe 工具 → tokio::spawn 立即执行
                  │
                  └── 是 unsafe 工具 → 暂存 pending unsafe queue（保留串行语义）
            │
[LLM stream 结束]
     │
     ├── 所有 safe 工具结果就绪（或仍在跑）
     │
     ├── 按 tool_use 原顺序逐个 join：
     │     │
     │     ├── safe 工具：已 spawn → 等结果
     │     │
     │     └── unsafe 工具：现在串行执行
     │
     └── 拼 follow-up message → 下一 step
```

**关键不变量**：
- **unsafe 工具仍串行**（保留写盘冲突 / approval gate 顺序）
- **tool_results 顺序严格按 LLM tool_use 顺序拼回**（API 协议要求）
- **cancel 触发时**：spawn 出去的 tool future 通过 cancel_token 立即 abort

### 2.2 当前协议的限制

Miragenty `llm/types.rs` 的 `StreamChunkKind`：

```rust
pub enum StreamChunkKind {
    TextDelta,
    ReasoningDelta,
    ToolUseStart { id: String, name: String },     // 🟢 有
    ToolUseInputDelta,                              // 🟡 有但 content 未带 tool_use_id
    MessageStop,
}
```

**第一个问题**：`ToolUseInputDelta` 不带 `tool_use_id`，无法区分多个并行 tool_use 的 input 分别属于谁。当前 forwarder（`engine.rs:701-755`）只过滤 Text/Reasoning，**所有 ToolUse 相关 chunk 都被 continue 丢弃**——`engine.rs:732`：

```rust
let kind_str = match chunk.kind {
    StreamChunkKind::TextDelta => "text_delta",
    StreamChunkKind::ReasoningDelta => "reasoning_delta",
    _ => continue,   // ← ToolUse* 完全没用到
};
```

**第二个问题**：tool_use_id / name / input 当前从**最终的 `LlmResponse.content` 解析**（`engine.rs:1014-1021`），不从 stream chunks 解析。

**这两个问题决定了 P1-1 不是 "engine.rs 局部改造" 而是 "贯穿 llm + engine 的协议升级"**。

### 2.3 改造范围

| 层 | 改动 |
| --- | --- |
| `llm/types.rs` | `StreamChunkKind` 改造：把 `ToolUseStart` / `ToolUseInputDelta` 改成带 `tool_use_id` 的结构，加 `ToolUseStop { tool_use_id }` |
| `llm/openai_compat.rs` | `stream_chat` 解析 OpenAI tool_calls delta 时同步 emit 新 chunk |
| `llm/anthropic.rs` | 同上，按 Anthropic content_block_start/delta/stop 事件 emit |
| `agent/streaming_tool_executor.rs` | **新建**：维护 pending queue + spawn + result channel |
| `agent/engine.rs` | forwarder 识别 tool_use chunks → 喂 executor；step 末尾按顺序 join |

---

## 3. 设计细节

### 3.1 协议升级 (`llm/types.rs`)

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StreamChunkKind {
    TextDelta,
    ReasoningDelta,
    /// 一个新的 tool_use block 开始。
    ToolUseStart {
        tool_use_id: String,
        name: String,
    },
    /// 该 tool_use_id 的 input JSON 的增量片段。
    ToolUseInputDelta {
        tool_use_id: String,
    },
    /// 该 tool_use_id 的 input 完整结束（input JSON 闭合）。
    /// 注意：这是**解析层**的事件，不一定对应 SSE 的物理 stop ——
    /// caller 拿到此事件即可认为 input 完整可执行。
    ToolUseStop {
        tool_use_id: String,
    },
    MessageStop,
}
```

**兼容性**：`StreamChunk.content` 字段保留——TextDelta / ReasoningDelta / ToolUseInputDelta 都用它装文本内容。

### 3.2 Provider 改造

#### 3.2.1 openai_compat.rs

当前 `stream_chat` 累积 `tool_calls: Vec<(id, name, args)>` 然后最终一次性返回。改造：

```rust
// 解析 OpenAI delta tool_call 时
for tc in delta_tool_calls {
    let idx = tc["index"].as_u64().unwrap_or(0) as usize;
    // 确保 tool_calls[idx] 槽位存在
    while tool_calls.len() <= idx {
        tool_calls.push((String::new(), String::new(), String::new()));
    }
    let slot = &mut tool_calls[idx];
    if let Some(id) = tc["id"].as_str() {
        if slot.0.is_empty() {
            slot.0 = id.to_string();
            // 第一次见到 id → emit ToolUseStart
            if let Some(name) = tc["function"]["name"].as_str() {
                slot.1 = name.to_string();
                let _ = tx.send(StreamChunk {
                    kind: StreamChunkKind::ToolUseStart {
                        tool_use_id: slot.0.clone(),
                        name: slot.1.clone(),
                    },
                    content: String::new(),
                }).await;
            }
        }
    }
    if let Some(args_delta) = tc["function"]["arguments"].as_str() {
        slot.2.push_str(args_delta);
        // emit InputDelta
        let _ = tx.send(StreamChunk {
            kind: StreamChunkKind::ToolUseInputDelta { tool_use_id: slot.0.clone() },
            content: args_delta.to_string(),
        }).await;
    }
}
// stream 结束时
for (id, _name, _args) in &tool_calls {
    let _ = tx.send(StreamChunk {
        kind: StreamChunkKind::ToolUseStop { tool_use_id: id.clone() },
        content: String::new(),
    }).await;
}
```

**关键点**：OpenAI 协议**不保证** stream 内 tool_call 完整性 —— 一个 tool_call 的 args 可能跨多个 chunk。`ToolUseStop` 由 client 在 finish_reason 抵达时主动 emit，而不是 server 直接给。

#### 3.2.2 anthropic.rs

Anthropic stream 有清晰的 content_block_start / content_block_delta / content_block_stop 事件，直接 1:1 映射。

### 3.3 StreamingToolExecutor (`agent/streaming_tool_executor.rs`)

```rust
//! Phase 2.1 (Single-Agent Uplift) 的演进：把"批量并行"升级为"流式并行"。
//!
//! 关键改动：
//!   - 之前：等 LlmResponse 全部返回 → 全部 tool_use 解析完 → 按桶执行
//!   - 现在：流式 chunk 抵达时即触发 spawn；step 末尾按原顺序 join
//!
//! Unsafe 桶仍串行（与现状一致）。Safe 桶现在 in-flight overlap with LLM stream。

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

pub struct StreamingToolExecutor {
    /// 已 spawn 的 safe 工具：tool_use_id → handle
    inflight: HashMap<String, JoinHandle<crate::tools::ToolOutput>>,
    /// 累积中的 input JSON：tool_use_id → (name, accumulated_input_string)
    pending: HashMap<String, (String, String)>,
    /// 已 spawn 的工具按 tool_use_id 顺序（用于 step 末尾 join 时校验顺序）
    spawn_order: Vec<String>,
    /// 暂存的 unsafe 工具：等 stream 结束后再串行跑
    unsafe_queue: Vec<(String, String, serde_json::Value)>,
    /// 执行入口（由 caller 注入，避免 executor 自己持有 agent context）
    dispatcher: Arc<dyn ToolDispatcher>,
}

#[async_trait::async_trait]
pub trait ToolDispatcher: Send + Sync {
    async fn dispatch(&self, agent_id: &str, name: &str, input: &serde_json::Value)
        -> crate::tools::ToolOutput;
    fn is_safe(&self, name: &str) -> bool;
}

impl StreamingToolExecutor {
    pub fn new(dispatcher: Arc<dyn ToolDispatcher>) -> Self {
        Self {
            inflight: HashMap::new(),
            pending: HashMap::new(),
            spawn_order: Vec::new(),
            unsafe_queue: Vec::new(),
            dispatcher,
        }
    }

    /// 收到 ToolUseStart chunk 时调用。
    pub fn on_tool_use_start(&mut self, tool_use_id: String, name: String) {
        self.pending.insert(tool_use_id, (name, String::new()));
    }

    /// 收到 ToolUseInputDelta chunk 时调用。
    pub fn on_input_delta(&mut self, tool_use_id: &str, delta: &str) {
        if let Some((_, acc)) = self.pending.get_mut(tool_use_id) {
            acc.push_str(delta);
        }
    }

    /// 收到 ToolUseStop chunk 时调用——尝试解析 input + 决定 spawn / queue。
    /// 返回 Some(error) 仅当 JSON 解析失败需要 caller 知道（极少）。
    pub fn on_tool_use_stop(&mut self, agent_id: &str, tool_use_id: &str) -> Option<String> {
        let (name, raw_args) = match self.pending.remove(tool_use_id) {
            Some(v) => v,
            None => return Some(format!("ToolUseStop for unknown tool_use_id={tool_use_id}")),
        };
        // 复用 openai_compat 的解析器逻辑（含 ARG_PARSE_ERROR sentinel）
        let input = crate::llm::parse_tool_arguments_or_sentinel(&name, &raw_args);

        if self.dispatcher.is_safe(&name) {
            let dispatcher = self.dispatcher.clone();
            let agent_id = agent_id.to_string();
            let tool_use_id_clone = tool_use_id.to_string();
            let name_clone = name.clone();
            let input_clone = input.clone();
            let handle = tokio::spawn(async move {
                dispatcher.dispatch(&agent_id, &name_clone, &input_clone).await
            });
            self.inflight.insert(tool_use_id.to_string(), handle);
            self.spawn_order.push(tool_use_id.to_string());
        } else {
            self.unsafe_queue.push((tool_use_id.to_string(), name, input));
        }
        None
    }

    /// step 末尾调用：按 ordered_tool_use_ids 顺序拿结果，unsafe 串行跑。
    /// `ordered_tool_use_ids` 来自最终 LlmResponse.content 解析——保证顺序与 follow-up 拼回一致。
    pub async fn drain_in_order(
        &mut self,
        agent_id: &str,
        ordered_tool_use_ids: &[String],
    ) -> Vec<(String, crate::tools::ToolOutput)> {
        let mut results = Vec::with_capacity(ordered_tool_use_ids.len());
        // unsafe 桶在最后串行执行——给 safe 桶最大 overlap 窗口
        // unsafe 顺序按 LLM tool_use 顺序（unsafe_queue 内部已按到达顺序排）
        for tu_id in ordered_tool_use_ids {
            if let Some(handle) = self.inflight.remove(tu_id) {
                let output = handle.await.unwrap_or_else(|e| crate::tools::ToolOutput {
                    content: format!("tool execution panicked: {e}"),
                    is_error: true,
                });
                results.push((tu_id.clone(), output));
            } else if let Some(idx) = self.unsafe_queue.iter().position(|(id, _, _)| id == tu_id) {
                let (_id, name, input) = self.unsafe_queue.remove(idx);
                let output = self.dispatcher.dispatch(agent_id, &name, &input).await;
                results.push((tu_id.clone(), output));
            } else {
                // pending 还没 stop？理论不应出现，防御性：跑现 input 即可
                results.push((tu_id.clone(), crate::tools::ToolOutput {
                    content: format!("internal: tool_use_id {tu_id} not in any queue"),
                    is_error: true,
                }));
            }
        }
        results
    }

    /// cancel 时显式 abort 所有 inflight。
    pub fn abort_all(&mut self) {
        for (_id, handle) in self.inflight.drain() {
            handle.abort();
        }
        self.pending.clear();
        self.unsafe_queue.clear();
    }
}
```

### 3.4 engine.rs 集成

#### 3.4.1 forwarder 改造

```rust
// engine.rs:695 forwarder spawn 之前
let executor = Arc::new(Mutex::new(StreamingToolExecutor::new(dispatcher.clone())));
let executor_for_fwd = executor.clone();
let agent_id_for_fwd = agent_id.to_string();
```

forwarder 内：

```rust
while let Some(chunk) = rx.recv().await {
    // ... 现有 text/reasoning 透传逻辑
    match &chunk.kind {
        StreamChunkKind::ToolUseStart { tool_use_id, name } => {
            executor_for_fwd.lock().await.on_tool_use_start(tool_use_id.clone(), name.clone());
            // emit tool_use_started 事件供 UI 提前显示
            let _ = app_handle.emit("agent-event", AgentEventPayload {
                agent_id: agent_id_for_fwd.clone(),
                step: stream_step,
                kind: "tool_use_started".to_string(),
                content: format!("{name}(...)"),
                meta: Some(serde_json::json!({ "tool_use_id": tool_use_id, "tool": name })),
            });
        }
        StreamChunkKind::ToolUseInputDelta { tool_use_id } => {
            executor_for_fwd.lock().await.on_input_delta(tool_use_id, &chunk.content);
        }
        StreamChunkKind::ToolUseStop { tool_use_id } => {
            if let Some(err) = executor_for_fwd.lock().await.on_tool_use_stop(&agent_id_for_fwd, tool_use_id) {
                tracing::warn!("streaming executor: {err}");
            }
        }
        _ => continue,
    }
}
```

#### 3.4.2 step 末尾 drain

替换现 `engine.rs:1224-1345` 整个 "for tool_use_blocks in safe_flags / 串行 unsafe" 块：

```rust
// 从 response.content 取出 tool_use_id 顺序
let ordered_ids: Vec<String> = tool_use_blocks.iter().map(|(id, _, _)| id.clone()).collect();
let mut tool_results_raw = executor.lock().await.drain_in_order(agent_id, &ordered_ids).await;

// 把结果按原顺序填回 follow-up builder
let mut followup = ToolFollowupBuilder::with_capacity(tool_use_blocks.len());
for (tu_id, output) in tool_results_raw.drain(..) {
    // emit tool_result 事件（同现状）
    let event_kind = if output.is_error { "error" } else { "tool_result" };
    self.emit_event_with_meta(agent_id, step, event_kind, &output.content,
        Some(serde_json::json!({ "tool_use_id": tu_id, "is_error": output.is_error })));
    followup.push_tool_result(tu_id, output.content, output.is_error);
}
```

#### 3.4.3 ToolDispatcher impl

```rust
struct EngineToolDispatcher {
    engine_ref: /* 弱引用 self 或重组接口 */,
}

#[async_trait::async_trait]
impl ToolDispatcher for EngineToolDispatcher {
    async fn dispatch(&self, agent_id: &str, name: &str, input: &serde_json::Value)
        -> ToolOutput
    {
        self.engine_ref.dispatch_tool(agent_id, name, input).await
    }
    fn is_safe(&self, name: &str) -> bool {
        crate::tools::lookup_tool_spec(name).map(|s| s.is_concurrency_safe).unwrap_or(false)
    }
}
```

由于 `dispatch_tool` 借 `&self`，最简方案：把 dispatcher 设计成持有 `Arc<ToolExecutor>` 而非 `&AgentEngine`，避免循环引用。

### 3.5 协议向后兼容

- 旧 provider（不 emit 新 chunk）：forwarder 收不到 ToolUseStart → executor 一直 empty → step 末尾 drain 时所有 ids 都不在 inflight/unsafe → 走"fallback 直接 dispatch"路径（保留旧行为）。
- 新 provider 没全量适配：渐进式启用。

### 3.6 Cancel 联动

`engine.rs` 现有 cancel check 已在每 step 边界 + stream_guard 内。改造后：

- cancel 触发 → stream_guard abort → forwarder 自然结束 → executor 持有的 inflight 通过 `abort_all()` 显式 abort
- 在 cancel handler 里加 `executor.lock().await.abort_all()`

---

## 4. 验收

### 4.1 单元测试 (`streaming_tool_executor.rs`)

```rust
#[tokio::test]
async fn safe_tool_spawns_immediately_on_stop() {
    let dispatcher = MockDispatcher::with_responses(vec![
        ("read_file", ToolOutput { content: "ok".into(), is_error: false }),
    ]);
    dispatcher.set_safe("read_file");
    let mut ex = StreamingToolExecutor::new(Arc::new(dispatcher));

    ex.on_tool_use_start("t1".into(), "read_file".into());
    ex.on_input_delta("t1", r#"{"path":"a.txt"}"#);
    ex.on_tool_use_stop("agent-1", "t1");

    // 此时 inflight 已经有 t1，不需要等 drain 就被 spawn
    // 给 spawn 一点时间真跑
    tokio::time::sleep(Duration::from_millis(20)).await;

    let results = ex.drain_in_order("agent-1", &["t1".to_string()]).await;
    assert_eq!(results.len(), 1);
    assert!(!results[0].1.is_error);
}

#[tokio::test]
async fn unsafe_tool_queued_until_drain() {
    // 类似上面，但 is_safe=false
    // 断言：drain 之前 dispatcher.call_count == 0
    // drain 后 == 1
}

#[tokio::test]
async fn out_of_order_chunks_still_correct() {
    // 多个 tool_use 交错 chunks：t1 start, t2 start, t1 delta, t2 delta, t2 stop, t1 stop
    // 断言 drain 返回顺序按 ordered_ids 而非完成顺序
}

#[tokio::test]
async fn abort_all_cancels_inflight() {
    // spawn 一个会 sleep 1s 的 fake tool
    // abort_all 后立即 drain，断言 result.is_error=true 或干脆少一条
}
```

### 4.2 集成测试

```rust
async fn test_engine_streams_tool_use_overlapped_with_llm() {
    // StubProvider：5s 内分多次 emit ToolUseStart(t1, read_file) → InputDelta → ...
    //                同时 emit text deltas
    // MockToolExecutor：read_file 耗 1s
    // 期望：整 step 耗时接近 5s 而非 6s
}
```

### 4.3 手动 E2E

跑一个 "读 5 文件 + 总结" 任务，比较 timeline 时间戳：

- 现状：tool_use_started 事件全部在 5.0s+ 出现，tool_result 在 5.3s+
- 改造后：tool_use_started 事件分布在 2-5s 范围内，tool_result 紧随其后

---

## 5. 风险

| 风险 | 缓解 |
| --- | --- |
| OpenAI delta tool_call 协议碎片化（不同 reseller 行为不一致） | 单测覆盖 OpenAI / DeepSeek / 通义 各家实测的 delta 格式；fallback 路径保留 |
| Safe 工具 spawn 后未 join（cancel 时泄漏） | `abort_all` 显式 abort + cancel 联动测试 |
| `dispatch_tool` 借用 `&self` 不能直接 spawn | dispatcher trait 抽出 Arc<ToolExecutor>，避免持有 engine |
| 流式协议升级影响其它已有 caller（planner / evaluator） | 这些 caller 也用 stream_chat 但都不需要 streaming tool execution（无 tool 或单一） → 新 chunk 它们直接忽略，无破坏 |
| Approval gate 顺序乱（unsafe 工具流式来时如果还有 pending approval） | unsafe 仍在 drain 阶段串行，approval gate 顺序与之前一致 |

---

## 6. 落地清单

| 文件 | 变更 | 行数 |
| --- | --- | --- |
| `src-tauri/src/llm/types.rs` | `StreamChunkKind` 升级（带 tool_use_id） | ~20 |
| `src-tauri/src/llm/openai_compat.rs` | stream_chat emit 新 chunk | ~60 |
| `src-tauri/src/llm/anthropic.rs` | 同上（Anthropic 协议） | ~50 |
| `src-tauri/src/llm/mod.rs` | re-export `parse_tool_arguments_or_sentinel` 给 executor 用 | 2 |
| `src-tauri/src/agent/streaming_tool_executor.rs` | **新建**：executor + dispatcher trait + 4 个单测 | ~280 |
| `src-tauri/src/agent/engine.rs` | (a) forwarder 加 tool_use chunk 处理; (b) step 末尾改 drain; (c) cancel 联动 | ~150 |
| `src-tauri/src/agent/engine.rs` 测试 | 1-2 个集成测试 | ~80 |
| `src/components/workspace/EventList.tsx` | `tool_use_started` 事件渲染（占位状态） | ~30 |

合计 ~670 行。**P1 中最重的一条**。

### commit 计划

```
feat(llm): emit per-tool_use stream chunks for streaming dispatch

Upgrade StreamChunkKind so providers emit ToolUseStart / InputDelta /
ToolUseStop with tool_use_id. Allows downstream consumers to start
dispatching tools as the stream arrives instead of waiting for the
full LlmResponse.

Backwards compatible: old consumers that match on TextDelta/Reasoning
ignore the new variants.
```

```
feat(agent): add StreamingToolExecutor for in-stream tool dispatch

Safe tools (read/search/list/glob) are spawned the moment their
input JSON closes mid-stream. Unsafe tools (write/edit/shell) remain
serialized in drain order. Cancel propagates via abort_all.

Module: FM-02
```

```
test(agent): cover streaming executor ordering, cancel, and unsafe queue
```

```
feat(ui): render tool_use_started placeholder during stream
```

---

## 7. 与其它 P0/P1 的关系

- **P0-1/P0-3**：reactive compact / recovery 流程不变；它们处理的是 step 整体错误，与 step 内部的 streaming 不冲突
- **P1-2 Cross-model Fallback**：fallback 触发会 `executor.abort_all()` 清空 → fallback 后用新 model 重发，executor 空状态启动
- **P1-3 max_output_tokens**：撞顶时 tool_use input 可能截断 → executor 收到 InputDelta 但永远没 Stop → drain 时按 fallback "直接走最终解析" 路径，与现状一致

---

## 8. 不在本 PR 内

1. **Tool 执行进度流式**：tool 自身长任务的进度（shell_exec 实时输出）属于另一条线（已有 `tool_progress` 事件），与 streaming dispatch 是正交问题
2. **Sub-tool spawn**：tool 调 tool（subagent 模式）不在范围
3. **MCP refresh**：动态加载 MCP 工具是另一条改造
