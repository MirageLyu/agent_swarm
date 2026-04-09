# FM-10.5: Context Compression — 上下文压缩

> 版本: v1.1 | 日期: 2026-04-09  
> 优先级: **P2 (Micro-compact) + P3 (Full Compaction)** | 预估周期: 2 天  
> 依赖: FM-10.1 (tool_use 格式), FM-10.2 (Belief State), FM-10.3 (Dynamic Prompt) | 被依赖: 无  
> 调研来源: Claude Code 架构分析 §维度5 + §模式3 + §模式5; Anthropic Context Engineering (2025.09); JetBrains Research 2025.12; Agent-LLM 协同机制调研 §1.2 (Token 用量反馈)

---

## 1. 目标

解决 Pre-flight 长对话（>8 轮）中的 **context rot** 问题——随着对话历史增长，LLM 注意力准确性下降，且 token 成本线性增长。通过两层压缩策略：

1. **Micro-compact (P2)**：清理历史轮次中 `present_choices` 工具结果的详细选项描述，仅保留用户选择结果
2. **Full Compaction (P3)**：当对话历史接近 context window 限制时，用 LLM 生成结构化摘要替代完整历史

---

## 2. 现状分析

| 维度 | 当前实现 | 问题 |
|------|----------|------|
| 对话历史 | 全量存储、全量传入 LLM | 第 8 轮时 ~4000 tokens，第 15 轮时 ~8000 tokens |
| Token 效率 | 无优化 | 历史选项的详细描述对后续轮无价值 |
| 信号密度 | 无筛选 | 早期轮次的问候、元对话稀释有效信息 |
| 长会话支持 | 无上限保护 | 超过 context window 时直接报错 |

---

## 3. 功能需求

### 第一层: Micro-compact（轻量清理）

#### FR-10.5.1: Tool Result 清理

- **FR-10.5.1a**: 定义可压缩的工具结果类型集合：`{ "present_choices" }`
- **FR-10.5.1b**: 对话历史中，距当前轮 > 3 轮的 `present_choices` tool_result，将详细选项列表替换为精简格式：
  ```
  [选项详情已压缩] 问题: "{question}" | 用户选择: "{selected_label}"
  ```
- **FR-10.5.1c**: 保留 `tool_use` 调用记录（函数名、参数摘要），仅清理 `tool_result` 的详细内容
- **FR-10.5.1d**: `add_contract_item` 和 `suggest_sign` 的 tool_result **不压缩**（内容短且重要）

#### FR-10.5.2: 清理触发时机

- **FR-10.5.2a**: 每次 `preflight_chat` 构建消息列表时，在发送给 LLM 前执行 micro-compact
- **FR-10.5.2b**: 原始对话历史在 DB (`preflight_sessions.messages`) 中保持完整不变
- **FR-10.5.2c**: Micro-compact 仅作用于发送给 LLM 的消息副本

#### FR-10.5.3: 元对话清理

- **FR-10.5.3a**: 距当前轮 > 5 轮的 agent 消息中，删除问候语和过渡性文本（如"好的，让我们继续"、"很高兴您选择了..."），保留核心分析内容
- **FR-10.5.3b**: 清理规则使用正则匹配前缀模式，不使用 LLM

### 第二层: Full Compaction（完整压缩）

#### FR-10.5.4: 触发条件

> 更新 (v1.1): 优先使用 API 返回的实际 token 数驱动触发判断，而非字符估算。  
> 来源: Agent-LLM 协同机制调研 §1.2 — Token 用量反馈驱动 compaction 触发和预算控制。

- **FR-10.5.4a**: 优先使用上一轮 LLM 响应中 `usage.input_tokens` 的实际值判断是否接近 context window 上限。当 `input_tokens ≥ effective_context_window × 0.70` 时触发 compaction
- **FR-10.5.4a'**: 若 API 未返回 `usage` 字段（部分 provider 不支持），退化为字符级估算：`chars × 1.5`（中文）或 `words × 1.3`（英文）
- **FR-10.5.4b**: 当对话轮次 ≥ 12 时触发（即使 token 数未达阈值）
- **FR-10.5.4c**: `querySource` 为 `compact` 时不再触发（防死循环）
- **FR-10.5.4d**: 连续 3 次 compaction 失败后熔断，不再尝试

#### FR-10.5.4.1: Token 预算追踪

- **FR-10.5.4.1a**: 每轮 `preflight_chat` 完成后，从 API 响应中提取并持久化以下指标到 `preflight_sessions`：
  - `last_input_tokens`: 上一轮实际输入 token 数
  - `last_output_tokens`: 上一轮实际输出 token 数
  - `cumulative_input_tokens`: 累计输入 token 数
  - `cumulative_output_tokens`: 累计输出 token 数
- **FR-10.5.4.1b**: 前端 `PreflightStatusBar` 可选显示 token 用量信息（调试模式下）
- **FR-10.5.4.1c**: 当 `last_input_tokens ≥ effective_context_window × 0.55` 时，在 `PreflightStatusBar` 提示"上下文较长，建议尽快完成澄清"

#### FR-10.5.5: 压缩 Prompt

调用 LLM 生成结构化摘要，prompt 模板：

```
请将以上 Pre-flight 澄清对话压缩为结构化摘要。

当前 Contract 状态（已独立持久化，无需在摘要中复述条目详情）：
- Scope: {scope_count} 条
- Constraints: {constraints_count} 条
- Exclusions: {exclusions_count} 条
- Assumptions: {assumptions_count} 条

请按以下结构输出，仅输出文本，不要调用任何工具：

1. 用户原始需求：[原文引用]
2. 关键决策及理由：[决策 → 理由 列表，最多 10 条]
3. 仍待澄清的问题：[列表]
4. 用户偏好与风格：[观察到的沟通偏好、技术倾向]
5. 对话中明确被否决的方案：[列表，防止 Agent 重复建议]
```

- **FR-10.5.5a**: 压缩请求使用 `max_tokens: 1500`
- **FR-10.5.5b**: 压缩请求不携带 tools 参数
- **FR-10.5.5c**: 压缩结果作为 user 消息插入到新的消息列表开头

#### FR-10.5.6: 压缩后的消息列表

```
[压缩边界标记]
[摘要 user 消息] ← 包含结构化摘要
[保留的最近 3 轮完整消息]
[当前轮 user 消息]
```

- **FR-10.5.6a**: 摘要消息前缀：`"本次对话是对之前澄清的延续。以下是之前讨论的结构化摘要：\n\n"`
- **FR-10.5.6b**: 始终保留最近 3 轮的完整消息（含 tool_use/tool_result）
- **FR-10.5.6c**: 用户原始需求消息始终保留在摘要之后

#### FR-10.5.7: Compaction 记录

- **FR-10.5.7a**: 压缩事件记录到 `preflight_sessions`：`compacted_at` 轮次 + `compaction_summary` 文本
- **FR-10.5.7b**: 完整对话日志始终保留在 DB 中，摘要标注 `[完整对话可在 session 日志中查看]`
- **FR-10.5.7c**: 压缩前后的 token 数差异记录到结构化日志

#### FR-10.5.8: Fallback 机制

- **FR-10.5.8a**: 压缩请求超时（>30s）时，退化为截断策略：删除最早 50% 的消息，保留最近 50%
- **FR-10.5.8b**: 压缩结果为空或格式异常时，使用截断策略
- **FR-10.5.8c**: 熔断后记录 `tracing::error`，不影响正常对话

---

## 4. 非功能需求

| ID | 类型 | 要求 |
|----|------|------|
| NFR-10.5.1 | 性能 | Micro-compact 处理延迟 ≤ 5ms |
| NFR-10.5.2 | 性能 | Full compaction 总延迟 ≤ 15s（含 LLM 调用） |
| NFR-10.5.3 | 数据完整性 | 原始对话历史在 DB 中永不修改 |
| NFR-10.5.4 | 可恢复性 | 压缩失败不影响当前对话 |
| NFR-10.5.5 | 可观测性 | 每次 micro-compact/compaction 记录清理的 token 数 |

---

## 5. 效果度量

### 5.1 Micro-compact 定量指标

| 指标 | 度量方法 | 基线（当前） | 目标 | 采集方式 |
|------|----------|-------------|------|----------|
| **Token 节省率** | `(压缩前 token - 压缩后 token) / 压缩前 token`（第 6 轮起） | 0% | ≥ 25% | 后端日志：每轮记录 micro-compact 前后 token 数 |
| **信息损失率** | 压缩后 LLM 仍能正确引用已确认 Contract 条目的比例 | N/A | ≥ 95% | 抽样测试：在第 8 轮问 LLM "你知道哪些已确认的决策？" |
| **处理延迟** | micro-compact 函数耗时 | 0ms | ≤ 5ms | 后端计时 |

### 5.2 Full Compaction 定量指标

| 指标 | 度量方法 | 基线（当前） | 目标 | 采集方式 |
|------|----------|-------------|------|----------|
| **压缩比** | `压缩后 token / 压缩前 token` | 100%（无压缩） | ≤ 40% | 后端日志 |
| **摘要覆盖率** | 摘要中包含的已确认决策 / 实际已确认决策 | N/A | ≥ 90% | 人工核对 |
| **压缩后对话质量** | 压缩后 3 轮内 Agent 是否重复提问已确认内容 | N/A | 重复率 ≤ 5% | 人工标注 |
| **Compaction 延迟** | LLM 生成摘要耗时 | N/A | ≤ 10s | 后端计时 |
| **支持的最大轮数** | 对话在不报错的前提下可持续的最大轮数 | ~15 轮（受 context window 限制） | ≥ 30 轮 | 端到端测试 |

### 5.3 信息保留质量验证

**实验设计**:
1. 用"实现用户认证系统"完成 8 轮 Pre-flight
2. 在第 9 轮时触发 full compaction
3. 在第 10 轮向 Agent 提问以下检测问题：
   - "我们之前确认了哪些功能范围？"
   - "有哪些方案被我否决了？"
   - "当前有什么未解决的问题？"

**度量**:
| 问题 | 通过标准 |
|------|----------|
| 确认的功能范围 | Agent 正确列出 ≥ 80% 的 confirmed scope items |
| 被否决的方案 | Agent 能提到 ≥ 1 个被明确否决的方案 |
| 未解决问题 | Agent 列出的问题与 Belief State unfilled slots 匹配 ≥ 70% |

### 5.4 长会话压力测试

| 场景 | 步骤 | 通过标准 |
|------|------|----------|
| 20 轮对话 | 持续对话 20 轮 | 无错误，Contract 质量不退化 |
| 30 轮对话 | 持续对话 30 轮（含至少 2 次 compaction） | 无错误，Agent 仍能引用关键决策 |
| 熔断保护 | 模拟 LLM 摘要生成连续 3 次失败 | 熔断生效，退化到截断策略，对话继续 |

---

## 6. 实现要点

### 6.1 后端改动

| 文件 | 改动 |
|------|------|
| `agent/planner.rs` | 新增 `micro_compact_messages()` 函数；在 `preflight_chat()` 构建消息列表时调用 |
| `agent/planner.rs` | 新增 `should_compact()` 判断函数；新增 `compact_preflight_history()` 压缩函数 |
| `agent/planner.rs` | 新增 `PREFLIGHT_COMPACT_PROMPT` 常量 |
| `llm/openai_compat.rs` | `stream_chat()` 支持 `max_tokens` 限制（压缩请求使用） |
| `db/migrations.rs` | 新增 `compacted_at` 和 `compaction_summary` 列到 `preflight_sessions` |

### 6.2 Micro-compact 算法

```rust
fn micro_compact_messages(
    messages: &[Message],
    current_round: u32,
    keep_recent: u32,  // 默认 3
) -> Vec<Message> {
    messages.iter().map(|msg| {
        if msg.role == "tool"
            && msg.tool_name == Some("present_choices")
            && msg.round < current_round - keep_recent
        {
            Message {
                content: format!(
                    "[选项详情已压缩] 问题: \"{}\" | 用户选择: \"{}\"",
                    msg.metadata.question,
                    msg.metadata.selected_label
                ),
                ..msg.clone()
            }
        } else {
            msg.clone()
        }
    }).collect()
}
```

---

## 7. 风险

| 风险 | 概率 | 影响 | 缓解 |
|------|------|------|------|
| Micro-compact 清理了关键信息 | 低 | 中 | 仅清理选项详情，保留选择结果和 Contract 条目 |
| Full compaction 摘要遗漏关键决策 | 中 | 高 | 压缩后 Belief State 独立注入，不依赖摘要 |
| 压缩请求增加额外 LLM 调用成本 | 确定 | 低 | 压缩请求 max_tokens=1500，远低于正常对话 |
| 压缩延迟影响用户体验 | 中 | 中 | 压缩期间显示"正在优化对话上下文..."提示 |
