# FM-10.1: Tool-as-Structure — 工具即结构

> 版本: v1.1 | 日期: 2026-04-09  
> 优先级: **P0** | 预估周期: 2-3 天  
> 依赖: FM-10 基础实现 | 被依赖: FM-10.2, FM-10.3, FM-10.5, FM-10.6  
> 调研来源: Claude Code 架构分析 §维度3 + §模式1; CALM (arXiv:2502.08820); Agent-LLM 协同机制调研 (2026-04-08)

---

## 1. 目标

用 OpenAI-compatible **function calling / tool_use** 替代当前基于 `---CHOICES---` 文本分隔符的结构化输出方案，从根本上解决：

1. **解析脆弱性**：LLM 不遵循分隔符格式时选项丢失
2. **单消息多问题不可控**：LLM 一次返回多组选项无法拆分
3. **Contract 更新依赖前端手动操作**：用户选择后需手动触发 `add_contract_item`

---

## 2. 现状分析

| 维度 | 当前实现 | 问题 |
|------|----------|------|
| 选项输出 | LLM 在回复末尾附加 `---CHOICES---\n[JSON]` | 分隔符遵从率 ~70%，fallback 解析不稳定 |
| Contract 条目添加 | 前端调用 `add_contract_item` command | 仅在用户选择包含 `contract_impact` 时触发，非结构化 |
| 签署建议 | 基于轮次计数的程序化指令 | 与 LLM 判断脱节，无结构化信号 |
| 模式切换 | 前端按钮 + 系统消息注入 | LLM 无法自主建议切换 |

---

## 3. 功能需求

### FR-10.1.1: Pre-flight 工具定义

在 LLM 请求中注册以下 5 个工具（OpenAI function calling 格式）：

| 工具名 | 职责 | 调用频率 |
|--------|------|----------|
| `present_choices` | 向用户展示结构化选项 | 每轮 0-1 次 |
| `add_contract_item` | 向 Contract 添加已确认条目 | 每轮 0-N 次 |
| `update_contract_item` | 修改已有 Contract 条目 | 偶尔 |
| `suggest_sign` | 建议签署 Contract | 最多 1 次 |
| `switch_clarification_mode` | 切换澄清模式 | 偶尔 |

**Schema 要求**：
- `present_choices.dimension` 必须为 `scope | constraints | exclusions | assumptions` 之一
- `present_choices.choices` 最少 2 项，每项必须有 `id` + `label`，可选 `description` 和 `impact`
- `add_contract_item.confidence` 必须为 `confirmed | tentative | inferred`
- `suggest_sign.readiness_assessment` 包含 `scope_completeness`、`constraints_completeness`、`risk_coverage` 三个 0-1 浮点数

### FR-10.1.2: 后端 tool_use 解析

- **FR-10.1.2a**: `preflight_chat()` 的 LLM 请求必须在 `tools` 参数中传入 5 个工具的 JSON Schema
- **FR-10.1.2b**: 解析 LLM 响应中的 `tool_calls` 数组（OpenAI 格式：`message.tool_calls[].function.name` + `.arguments`）
- **FR-10.1.2c**: 每个 `tool_call` 的 `arguments` 通过 `serde_json` 反序列化为对应 Rust 结构体
- **FR-10.1.2d**: 解析失败时记录 `tracing::warn` 并降级为纯文本响应（无选项、无 Contract 更新）

### FR-10.1.3: 后端 tool_result 构建

- **FR-10.1.3a**: `present_choices` 工具调用后，将选项列表通过 `preflight-stream` 事件的 `choices` 字段推送到前端，同时构建 `tool_result` 消息追加到对话历史，等待用户选择后填入结果
- **FR-10.1.3b**: `add_contract_item` 工具调用后，直接执行 DB 写入，构建 `tool_result = { success: true, item_id }` 并追加到对话历史
- **FR-10.1.3c**: `suggest_sign` 工具调用后，将签署建议推送到前端，前端展示签署确认 UI
- **FR-10.1.3d**: `switch_clarification_mode` 工具调用后，更新 session mode 并通知前端

### FR-10.1.4: 前端适配

- **FR-10.1.4a**: `preflight-stream` 事件新增 `tool_calls` 字段，前端根据 `tool_name` 分发处理
- **FR-10.1.4b**: `present_choices` 的渲染逻辑复用现有 `ChoiceButtons` 组件，数据源改为 tool_call 参数
- **FR-10.1.4c**: `add_contract_item` 的结果实时更新 `ContractPanel`（无需用户手动操作）
- **FR-10.1.4d**: `suggest_sign` 触发签署确认弹窗，用户可接受或拒绝

### FR-10.1.5: 文本 Fallback

- **FR-10.1.5a**: 当 LLM 响应不包含任何 `tool_calls` 时，保留现有 `---CHOICES---` 分隔符解析作为降级方案
- **FR-10.1.5b**: 当降级方案也失败时，尝试 `extract_choices_from_markdown()` 从 Markdown 列表提取选项
- **FR-10.1.5c**: Fallback 触发时记录 `tracing::info` 日志，便于监控工具遵从率

### FR-10.1.6: System Prompt 工具使用指引

- **FR-10.1.6a**: System prompt 中明确指示 LLM 必须使用工具输出结构化内容，禁止使用 `---CHOICES---`
- **FR-10.1.6b**: 说明每个工具的使用时机和约束
- **FR-10.1.6c**: 强调"每轮最多调用一次 `present_choices`"以避免单消息多问题

### FR-10.1.7: Response Prefilling（响应预填充）

> 来源: Agent-LLM 协同机制调研 §1.1 — 零成本提升 tool_use 遵从度

通过预填充 assistant 消息开头，引导 LLM 以工具调用格式响应，而非自由文本。

- **FR-10.1.7a**: 在 LLM 请求中追加一条 `role=assistant` 的 prefill 消息，内容为 tool_use 的 JSON 开头片段（如 `{"name":"`），引导模型直接输出工具调用
- **FR-10.1.7b**: 仅在模型能力注册表标记 `supports_prefill = true` 时启用（参见 FM-10.3 FR-10.3.9）
- **FR-10.1.7c**: Prefilling 与 streaming 模式兼容——prefill 内容不计入 stream 输出
- **FR-10.1.7d**: 当模型不支持 prefill 时，回退到仅依靠 system prompt 中的工具使用指引

**实现方式**:
```
// LLM 请求的消息列表末尾追加:
{ "role": "assistant", "content": "" }  // 部分空 prefill，触发模型续写
```

> 注意: DashScope 的 prefill 支持需验证。若 API 不支持 assistant prefill，
> 可在 system prompt 末尾追加 `"你的回复必须以工具调用开始"` 作为替代。

### FR-10.1.8: Parallel Tool Calls（并行工具调用）

> 来源: Agent-LLM 协同机制调研 §1.1 — 单轮多个独立工具并行执行  
> 优先级: **P3**（当前不实现，预留接口）

- **FR-10.1.8a**: 后端解析逻辑支持单次 LLM 响应中包含 **多个** `tool_calls`（如同时调用 `present_choices` + `add_contract_item`）
- **FR-10.1.8b**: 多个 tool_calls 按顺序执行，每个 tool_result 独立追加到对话历史
- **FR-10.1.8c**: 当前阶段 system prompt 仍指示"每轮最多调用一次 `present_choices`"，但不限制 `add_contract_item` 的并行调用次数（允许 LLM 在用户确认后一次性写入多个 Contract 条目）

---

## 4. 非功能需求

| ID | 类型 | 要求 |
|----|------|------|
| NFR-10.1.1 | 可靠性 | tool_use 解析成功率 ≥ 95%（基于 20 轮测试样本） |
| NFR-10.1.2 | 兼容性 | 必须兼容 OpenAI function calling 格式（DashScope qwen3.5-plus） |
| NFR-10.1.3 | 降级 | Fallback 链路 (tool_use → CHOICES 分隔符 → Markdown 提取) 确保 ≥ 99% 的可用性 |
| NFR-10.1.4 | 性能 | 工具 Schema 定义的固定 token 消耗 ≤ 1200 tokens |
| NFR-10.1.5 | 可维护性 | 工具 Schema 集中定义于单一模块，修改不需改动解析逻辑 |

---

## 5. 效果度量

### 5.1 定量指标

| 指标 | 度量方法 | 基线（当前） | 目标 | 采集方式 |
|------|----------|-------------|------|----------|
| **选项解析成功率** | `成功解析出 ≥1 个 choice 的 Agent 回复轮次 / 应包含选项的 Agent 回复总轮次` | ~70% | ≥ 95% | 后端日志：在 `parse_preflight_response` 中记录 `tool_use_parsed: true/false` 和 `fallback_used: text/markdown/none` |
| **单消息多问题率** | `包含 >1 次 present_choices 调用的 Agent 回复轮次 / 总轮次` | ~30%（不可控） | ≤ 5% | 后端日志：统计单次响应中 `present_choices` 调用次数 |
| **Contract 自动写入率** | `由 LLM tool_call 触发的 contract_item 写入次数 / 所有 contract_item 写入次数` | 0%（全手动） | ≥ 80% | DB 查询：`contract_items.source = 'agent'` 的比例 |
| **Fallback 触发率** | `使用文本 fallback 的轮次 / 总轮次` | 100%（当前唯一通道） | ≤ 10% | 后端日志：`fallback_used != 'none'` 的比例 |
| **工具 Schema token 开销** | 用 tiktoken 计算 5 个工具定义的 token 数 | 0 | ≤ 1200 tokens | 一次性计算 |
| **Prefilling 遵从度提升** | 开启/关闭 prefill 两组各 10 轮对比 tool_use 使用率 | 基线(无 prefill) | ≥ 5% 绝对提升 | A/B 测试 |

### 5.2 定性验证

| 验证项 | 方法 | 通过标准 |
|--------|------|----------|
| **qwen3.5-plus 工具遵从度** | 用 3 个不同复杂度的需求（简单/中等/复杂）各运行 5 轮 Pre-flight | 15 轮中 ≥ 13 轮正确使用 tool_use |
| **Fallback 链路健壮性** | 模拟 LLM 响应不含 tool_calls 的场景 | 降级到文本解析，不出现空选项 |
| **前端兼容性** | 分别验证 tool_use 路径和 fallback 路径的 UI 渲染 | 两种路径的用户体验一致 |

### 5.3 数据采集

- 后端在 `planner.rs` 的 `preflight_chat` 函数中新增结构化日志：
  ```
  tracing::info!(
      round = %round,
      tool_calls_count = %tool_calls.len(),
      tool_names = %tool_names_csv,
      fallback_used = %fallback_type,
      choices_parsed = %choices_count,
      "preflight round completed"
  );
  ```
- 前端在 `PreflightView` 中记录每轮的数据源（`tool_use` / `text_fallback` / `markdown_fallback`）到 session 日志

---

## 6. 实现要点

### 6.1 后端改动

| 文件 | 改动 |
|------|------|
| `agent/planner.rs` | 新增 `PREFLIGHT_TOOLS` 常量（5 个工具的 JSON Schema）；重构 `preflight_chat()` 在 LlmRequest 中传入 `tools` 参数；新增 `parse_tool_calls()` 函数；保留 `parse_preflight_response()` 作为 fallback |
| `llm/openai_compat.rs` | 确认 `stream_chat()` 支持在请求中携带 `tools` 参数并解析响应中的 `tool_calls` |
| `llm/types.rs` | 新增 `ToolCall`、`FunctionCall`、`ToolResult` 类型定义 |
| `commands/preflight.rs` | `send_preflight_message` 中处理 `tool_result` 回写 |

### 6.2 前端改动

| 文件 | 改动 |
|------|------|
| `views/PreflightView.tsx` | 处理 `tool_calls` 事件分发 |
| `components/preflight/PreflightChat.tsx` | 渲染逻辑适配 tool_use 数据源 |
| `components/preflight/ContractPanel.tsx` | 监听 `add_contract_item` tool_call 自动刷新 |
| `ipc/commands.ts` | 新增 `confirmToolResult` IPC 命令 |

---

## 7. 风险

| 风险 | 概率 | 影响 | 缓解 |
|------|------|------|------|
| qwen3.5-plus 工具遵从度不足 | 中 | 高 | 保留三层 Fallback 链路；Schema 尽量扁平，减少嵌套和 enum；启用 Response Prefilling 引导 |
| 工具 Schema 增加固定 token 消耗 | 确定 | 低 | 压缩 description 字段，目标 ≤ 1200 tokens |
| 流式输出中 tool_call 参数截断 | 低 | 中 | 在 streaming 结束后做完整 JSON 校验 |
| DashScope 不支持 assistant prefill | 中 | 低 | 回退到 system prompt 尾部追加格式指令 |
