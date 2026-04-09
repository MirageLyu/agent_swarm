# FM-10.3: Dynamic System Prompt Assembly — 动态系统提示拼装

> 版本: v1.1 | 日期: 2026-04-09  
> 优先级: **P1** | 预估周期: 2 天  
> 依赖: FM-10.1 (工具定义), FM-10.2 (Belief State) | 被依赖: FM-10.4, FM-10.5  
> 调研来源: Claude Code 架构分析 §维度2 + §模式2; Anthropic Context Engineering (2025.09); Agent-LLM 协同机制调研 (2026-04-08)

---

## 1. 目标

将当前静态的 Pre-flight system prompt 改造为 **多层动态拼装** 架构，使 LLM 在每轮对话中都能感知：

1. **Contract 当前状态**：哪些条目已确认、哪些待确认
2. **Belief State 快照**：槽位填充状态、收敛分数、当前阶段
3. **收敛指令**：当前阶段应采取的对话策略
4. **澄清模式指引**：场景走查 / 魔鬼代言人 / 风险标记的具体行为指南

同时保持 prompt 结构稳定，为 FM-10.4 (Prompt Caching) 创造缓存友好条件。

此外，引入 **Model Capability Registry** 抽象层，使 prompt 构建和响应解析根据模型能力动态适配，遵循 **"API 级机制优先于 prompt 技巧"** 的核心设计原则。

---

## 2. 现状分析

| 维度 | 当前实现 | 问题 |
|------|----------|------|
| System prompt | 静态模式 prompt + 硬编码轮次信息 | LLM 不知道 Contract 当前状态 |
| Contract 注入 | 无 | LLM 只能从对话历史中"推断"已确认内容 |
| 收敛指令 | 基于 `user_rounds >= 5/8` 硬编码 | 缺乏结构化上下文 |
| 模式切换 | 切换后发送系统消息 | 无模式专属行为指引 |
| 缓存友好性 | 无考虑 | 每轮全量变化，无法利用缓存 |

---

## 3. 功能需求

### FR-10.3.1: Prompt 分层架构

将 system prompt 分为 **静态前缀** 和 **动态后缀** 两部分，以 `__DYNAMIC_BOUNDARY__` 标记分界：

```
┌─────────────────────────────────────────────┐
│ 静态前缀 (Static Prefix)                      │
│ ┌─────────────────────────────────────────┐ │
│ │ § 角色定义 (role_definition)              │ │
│ │ § 对话策略 (dialogue_strategy)            │ │
│ │ § 工具使用规范 (tool_usage)               │ │
│ │ § 输出格式 (output_format)               │ │
│ └─────────────────────────────────────────┘ │
│ ═══ __DYNAMIC_BOUNDARY__ ═══                │
│ 动态后缀 (Dynamic Suffix)                    │
│ ┌─────────────────────────────────────────┐ │
│ │ § 当前模式 (clarification_mode)          │ │
│ │ § Contract 状态 (contract_state)         │ │
│ │ § Belief State (belief_state)            │ │
│ │ § 收敛指令 (convergence_directive)        │ │
│ │ § 轮次信息 (round_info)                  │ │
│ └─────────────────────────────────────────┘ │
└─────────────────────────────────────────────┘
```

**FR-10.3.1a**: 静态前缀在同一 Pre-flight session 中保持不变  
**FR-10.3.1b**: 动态后缀在每轮 `preflight_chat` 调用时重新生成  
**FR-10.3.1c**: 静态前缀与动态后缀之间以 `__DYNAMIC_BOUNDARY__` 分界标记

### FR-10.3.2: 静态前缀内容

```
你是 Miragenty 的 Pre-flight Planner Agent，负责通过多轮对话澄清需求，
构建 Mission Contract（Scope / Constraints / Exclusions / Assumptions）。

# 对话策略
- 每轮聚焦 1 个维度，使用 present_choices 工具提供结构化选项
- 用户确认后立即用 add_contract_item 写入 Contract
- 随着澄清深入，减少开放式问题，增加确认式问题
- 永远不要使用 ---CHOICES--- 分隔符，所有结构化输出通过工具完成
- 在工具调用之外的文本用于向用户解释推理过程

# 工具使用规范
- present_choices: 需要用户选择时调用，每轮最多 1 次
- add_contract_item: 用户确认后写入，标注 confidence
- update_contract_item: 后续讨论推翻了之前的假设时使用
- suggest_sign: 仅在收敛分数 > 85% 或 phase=ReadyToSign 时使用
- switch_clarification_mode: 当前模式效率低下时切换

# 输出规范
- 文本部分使用中文，保持简洁专业
- 每条消息文本 ≤ 300 字，避免冗长解释
```

**FR-10.3.2a**: 静态前缀不包含任何会话特定信息  
**FR-10.3.2b**: 静态前缀的 token 数控制在 400-600 tokens

### FR-10.3.3: 动态后缀 — Contract 状态段

```
# Contract 当前状态
{{contract_state_compact_json}}
```

**FR-10.3.3a**: Contract 状态用紧凑 JSON 表示，格式如：
```json
{"scope":["实现OAuth登录(confirmed)","支持GitHub/Google(tentative)"],
 "constraints":["使用React前端(confirmed)"],
 "exclusions":["不含支付(confirmed)"],
 "assumptions":[]}
```
**FR-10.3.3b**: Contract 条目超过 20 条时，只保留最近 5 轮新增/修改的条目全文，其余用 `"...及另外 N 条"` 省略  
**FR-10.3.3c**: Contract 状态段的 token 上限为 800 tokens

### FR-10.3.4: 动态后缀 — Belief State 段

```
# 信念状态
收敛分数: {{convergence_score}}%
当前阶段: {{phase}}
已确认 slot: {{confirmed_slots}}
待确认 slot: {{tentative_slots}}
未触及 slot: {{unfilled_slots}}
```

**FR-10.3.4a**: Slot 列表只显示名称，不含详细内容  
**FR-10.3.4b**: Belief State 段的 token 上限为 200 tokens

### FR-10.3.5: 动态后缀 — 收敛指令段

根据 FM-10.2 的 `get_convergence_directive()` 生成，内容随 phase 和 score 动态变化。

**FR-10.3.5a**: ReadyToSign 阶段的指令必须明确要求调用 `suggest_sign`  
**FR-10.3.5b**: 收敛指令段的 token 上限为 150 tokens

### FR-10.3.6: 动态后缀 — 澄清模式段

三种模式的专用指引：

| 模式 | 核心指引 | 关注重点 |
|------|----------|----------|
| scenario_walkthrough | 通过具体场景引导用户思考边界 | 用户旅程、边界场景、异常流程 |
| devils_advocate | 质疑假设，提出挑战和反例 | 隐含假设、技术风险、遗漏需求 |
| risk_highlighter | 识别并标记风险 | 性能瓶颈、安全风险、集成复杂度 |

**FR-10.3.6a**: 模式指引包含 3-5 个具体行为示例  
**FR-10.3.6b**: 模式指引段的 token 上限为 200 tokens

### FR-10.3.7: Prompt 组装引擎

后端新增 `build_preflight_system_prompt()` 函数：

```rust
fn build_preflight_system_prompt(
    mode: &PreflightMode,
    contract: &ContractState,
    belief_state: &PreflightBeliefState,
    round: u32,
) -> String
```

**FR-10.3.7a**: 静态前缀从常量加载，动态后缀通过模板引擎生成  
**FR-10.3.7b**: 输出的 prompt 总 token 数记录到结构化日志  
**FR-10.3.7c**: 总 prompt token 数超过 2000 tokens 时记录 `tracing::warn`

### FR-10.3.8: 项目上下文注入

参考 Claude Code 的 CLAUDE.md 模式，将项目上下文作为 user 消息注入：

**FR-10.3.8a**: 若用户在 Settings 中配置了"项目描述"或"技术栈偏好"，将其包装为 `<project-context>` 标签的 user 消息，prepend 到对话历史  
**FR-10.3.8b**: 项目上下文标记为"不一定与当前讨论相关"，避免 LLM 过度使用

### FR-10.3.9: Model Capability Registry（模型能力注册表）

> 来源: Agent-LLM 协同机制调研 §二 — 通过抽象层隔离模型差异

定义模型能力注册表，使 prompt 构建、响应解析、缓存策略均根据当前模型能力动态适配。

**FR-10.3.9a**: 定义 `ModelCapabilities` 数据结构：

```rust
struct ModelCapabilities {
    supports_thinking: bool,        // 是否支持 Extended Thinking API
    supports_tool_use: bool,        // 是否支持 function calling
    supports_prompt_caching: bool,  // 是否支持显式 prompt caching
    supports_prefill: bool,         // 是否支持 response prefilling
    supports_streaming: bool,       // 是否支持 SSE streaming
    supports_parallel_tools: bool,  // 是否支持并行工具调用
    supports_logprobs: bool,        // 是否返回 token 概率
    thinking_api_param: Option<String>,  // thinking 激活参数名 (如 "enable_thinking")
    cache_control_syntax: Option<String>, // 缓存语法 (如 "anthropic")
}
```

**FR-10.3.9b**: 内置主流模型的能力配置：

| 模型 | thinking | tool_use | caching | prefill | streaming |
|------|----------|----------|---------|---------|-----------|
| qwen3.5-plus (DashScope) | ❌ | ✅ | ✅ anthropic | 待验证 | ✅ |
| qwen3 (DashScope) | ✅ `enable_thinking` | ✅ | ✅ | 待验证 | ✅ |
| claude-4-sonnet (Anthropic) | ✅ `thinking.type` | ✅ | ✅ anthropic | ✅ | ✅ |
| gpt-4o (OpenAI) | ❌ | ✅ | ✅ auto | ❌ | ✅ |
| deepseek-r1 (DeepSeek) | ✅ `enable_thinking` | ✅ | ✅ auto | ❌ | ✅ |

**FR-10.3.9c**: 能力注册表可通过 Settings 界面自定义（用户添加自定义模型时填写能力配置）  
**FR-10.3.9d**: `build_preflight_system_prompt()` 接受 `ModelCapabilities` 参数，根据能力决定：
- `supports_thinking = true` → 不在 prompt 中添加 CoT 引导
- `supports_thinking = false` → 在 prompt 中添加 `<analysis>` 标签引导结构化推理
- `supports_tool_use = false` → 回退到 `---CHOICES---` 文本约定
- `supports_prefill = true` → 启用 Response Prefilling (FM-10.1.7)

### FR-10.3.10: Extended Thinking 适配

> 来源: Agent-LLM 协同机制调研 §二 — Extended Thinking 互斥规则

**核心约束**: 开启 Thinking API 时，**禁止**在 prompt 中使用 "think step by step"、"请先分析再回答" 等 Chain-of-Thought 引导语，否则模型会推理两遍（thinking block 一次 + text 中再一次），造成双倍 token 消耗和潜在矛盾。

**FR-10.3.10a**: Prompt 构建阶段的分支逻辑：

```
IF model.supports_thinking:
  - 从静态前缀中移除所有 CoT 引导（"请先分析"、"逐步思考" 等）
  - 在 LLM 请求参数中设置 thinking: { type: "enabled", budget_tokens: N }
  - N 根据当前阶段动态调整：Exploring=2048, Narrowing=1024, Confirming=512
ELSE:
  - 在静态前缀中保留 CoT 引导，使用 <analysis>...</analysis> 标签
  - 指示 LLM 在 <analysis> 中进行推理，然后在标签外输出结论和工具调用
```

**FR-10.3.10b**: 响应解析阶段的统一接口：

```rust
fn extract_reasoning(response: &LlmResponse, caps: &ModelCapabilities) -> Option<String> {
    if caps.supports_thinking {
        // 从 response.content 中提取 type="thinking" 的 block
        response.thinking_blocks().map(|b| b.text.clone())
    } else {
        // 从 text 中正则提取 <analysis>...</analysis>
        extract_between_tags(&response.text, "analysis")
    }
}
```

**FR-10.3.10c**: 提取的推理内容可用于：
- 检测 Agent 的置信度和犹豫点
- 辅助 Belief State 的 slot 自动分类（Agent 在推理中提到的维度）
- 调试和质量评估

### FR-10.3.11: 核心设计原则

以下原则贯穿整个 Prompt Assembly 引擎的设计：

1. **API 级机制优先于 Prompt 技巧**：模型原生支持的能力（Thinking、Caching、Tool Use）始终优先于 prompt 模拟。只在模型不支持原生能力时才使用 prompt fallback。
2. **能力检测驱动行为适配**：prompt 构建、响应解析、缓存策略均根据 `ModelCapabilities` 动态调整，上层业务逻辑不感知具体模型。
3. **静态稳定，动态灵活**：prompt 中不随对话变化的部分（角色定义、工具规范）保持字节级稳定以利于缓存，仅动态段按轮变化。

---

## 4. 非功能需求

| ID | 类型 | 要求 |
|----|------|------|
| NFR-10.3.1 | 性能 | Prompt 组装延迟 ≤ 2ms |
| NFR-10.3.2 | Token 预算 | 完整 system prompt (静态 + 动态) ≤ 2000 tokens |
| NFR-10.3.3 | 缓存友好 | 静态前缀在同一 session 中字节级一致 |
| NFR-10.3.4 | 可维护性 | 各段独立定义，修改单段不影响其他段 |
| NFR-10.3.5 | 可观测性 | 每次组装记录各段 token 数到结构化日志 |
| NFR-10.3.6 | 可扩展性 | 新增模型支持仅需配置 `ModelCapabilities`，无需修改 prompt 组装逻辑 |
| NFR-10.3.7 | 正确性 | Thinking 互斥检测：运行时断言 `supports_thinking && prompt_contains_cot` 不同时为 true |

---

## 5. 效果度量

### 5.1 定量指标

| 指标 | 度量方法 | 基线（当前） | 目标 | 采集方式 |
|------|----------|-------------|------|----------|
| **LLM 状态感知准确度** | 向 LLM 追加"你当前知道 Contract 中有哪些条目？"的隐式检测问题，比对回答与实际 | 不可量化 | ≥ 80% 的条目被正确引用 | 抽样测试（每 5 轮测一次） |
| **重复提问率** | `LLM 提问了已确认 slot 相关问题的轮次 / 总轮次` | ~20%（LLM 从对话历史推断，常遗忘） | ≤ 5% | 人工标注 15 轮样本 |
| **System prompt token 数** | tiktoken 计算每轮的 system prompt 总 token | ~300 (仅静态) | ≤ 2000 (含动态) | 后端日志 |
| **静态前缀稳定性** | 连续 10 轮中静态前缀字节级一致的次数 | N/A | 10/10 (100%) | 后端日志：hash 比较 |
| **Thinking 互斥合规性** | `supports_thinking=true` 时 prompt 中不含 CoT 引导 | N/A | 100% | 运行时断言 + UT |
| **能力适配覆盖率** | `ModelCapabilities` 中所有 bool 字段在 prompt 构建中均有对应分支 | N/A | 100% | 代码审查 |

### 5.2 定性验证

| 验证项 | 方法 | 通过标准 |
|--------|------|----------|
| **Agent 不重复已确认内容** | 在 Contract 已有 5+ 条目时继续对话 | Agent 不再追问已 Confirmed 的领域 |
| **模式切换后行为变化** | 从场景走查切换到魔鬼代言人 | Agent 立即转变为质疑式提问风格 |
| **收敛指令生效** | 在 ReadyToSign 阶段观察 Agent 行为 | Agent 主动总结并建议签署 |
| **Prompt 过长降级** | Contract 超 20 条目 | 省略机制生效，prompt 不超限 |

### 5.3 A/B 对比

用"实现用户认证系统"对比动态 prompt vs 静态 prompt：

| 维度 | 静态 prompt | 动态 prompt 目标 | 度量方式 |
|------|------------|----------------|----------|
| Agent 重复提问率 | ~20% | ≤ 5% | 人工标注 |
| 到达 ReadyToSign 的轮次 | ~10 轮 | ≤ 8 轮 | 自动记录 |
| 用户满意度 | 基准 | 提升 20%+ | 主观评分 |

---

## 6. 实现要点

### 6.1 后端改动

| 文件 | 改动 |
|------|------|
| `llm/types.rs` | 新增 `ModelCapabilities` 结构体定义 |
| `llm/registry.rs` (新) | 内置模型能力注册表，提供 `get_capabilities(provider, model)` 查询 |
| `agent/planner.rs` | 新增 `build_preflight_system_prompt()` 函数，接受 `ModelCapabilities` 参数；提取当前三组 prompt 到 `STATIC_PREFIX`；重构 `preflight_chat()` 调用新的组装函数 |
| `agent/planner.rs` | 新增 `compact_contract_json()` — 将 Contract 条目压缩为紧凑 JSON |
| `agent/planner.rs` | 新增 `render_belief_state_section()` — 渲染 Belief State 段 |
| `agent/planner.rs` | 新增 `extract_reasoning()` — 统一推理提取接口 |
| `commands/preflight.rs` | 若有项目上下文，构建 `<project-context>` user 消息 |
| `commands/config.rs` | 通过 `ModelCapabilities` 驱动 thinking 参数注入 |

### 6.2 Prompt 段注册表

```rust
struct PromptSection {
    name: &'static str,      // 段名
    is_static: bool,          // 是否为静态段
    max_tokens: usize,        // token 上限
    render: fn(&Context) -> String,  // 渲染函数
}
```

支持按段启用/禁用，便于调试和 A/B 测试。

---

## 7. 风险

| 风险 | 概率 | 影响 | 缓解 |
|------|------|------|------|
| 动态段增加 token 消耗 | 确定 | 低 | 紧凑 JSON + token 上限控制 |
| Contract JSON 在 prompt 中导致 LLM "复读" | 中 | 低 | 指令明确标注"仅作参考，不要复述" |
| 模式指引过于具体限制 LLM 灵活性 | 低 | 中 | 指引以"重点关注"而非"必须"措辞 |
| Prompt 模板引擎引入 bug | 低 | 中 | 全覆盖 UT 验证各种边界情况 |
| Thinking API 与 CoT prompt 同时存在 | 低 | 高 | 运行时断言 + `ModelCapabilities` 驱动的互斥检查 |
| 模型能力配置错误 | 中 | 中 | 内置安全默认值（全部 false），用户可覆盖 |
| Extended Thinking token 预算过高 | 低 | 低 | 按阶段动态调整 budget，Confirming 阶段大幅降低 |
