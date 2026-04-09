# Claude Code 架构分析 → Miragenty Pre-flight 优化调研报告

> 调研日期: 2026-04-08  
> 数据来源: Claude Code 源码仓库逆向分析 + Anthropic 官方博客 + 第三方逆向工程文章  
> 目标: 提取可用于优化 Miragenty Pre-flight 多轮澄清模式的关键设计模式

---

## 第一部分：Claude Code 架构解析

### 维度 1: Agentic Loop 与对话状态管理

#### 1.1 核心循环结构

Claude Code 的 agentic loop 位于 `src/query.ts`，核心是一个 **`while (true)` 无限循环**，通过 AsyncGenerator 模式驱动：

```typescript
// src/query.ts ~306
while (true) {
  // 1. 构建请求上下文 (messagesForQuery, tools, systemPrompt)
  // 2. 流式调用模型 API
  // 3. 检查 assistant 响应中是否包含 tool_use 块
  // 4a. 有 tool_use → 执行工具 → 拼接 tool_result → 继续循环
  // 4b. 无 tool_use → 触发 stop hooks → return { reason: 'completed' }
}
```

跨迭代的可变状态封装在 `State` 类型中：

```typescript
// src/query.ts ~204
type State = {
  messages: Message[]
  toolUseContext: ToolUseContext
  autoCompactTracking: AutoCompactTrackingState | undefined
  maxOutputTokensRecoveryCount: number
  hasAttemptedReactiveCompact: boolean
  maxOutputTokensOverride: number | undefined
  pendingToolUseSummary: Promise<ToolUseSummaryMessage | null> | undefined
  stopHookActive: boolean | undefined
  turnCount: number
  transition: Continue | undefined  // 上一轮为何继续（调试用）
}
```

#### 1.2 停止条件判定

Claude Code **不依赖 API 返回的 `stop_reason`**，而是以 **是否出现 `tool_use` 块** 作为核心信号：

```typescript
// src/query.ts ~551-558
// Note: stop_reason === 'tool_use' is unreliable
// Set during streaming whenever a tool_use block arrives — the sole
// loop-exit signal. If false after streaming, we're done.
let needsFollowUp = false
```

完整的终止条件矩阵：

| 条件 | 终止原因 | 位置 |
|------|----------|------|
| `needsFollowUp === false` 且无 recovery | `completed` | ~1357 行 |
| 达到 `maxTurns` 上限 | `max_turns` | ~1704 行 |
| AbortController 触发 | `aborted_streaming` / `aborted_tools` | ~1015/1484 行 |
| Hook 阻止继续 | `hook_stopped` | ~1518 行 |
| 阻塞 token 上限 | `blocking_limit` | ~641 行 |

#### 1.3 是否有 Belief State？

**没有显式的 belief state 或形式化状态机。** 状态完全隐含在：
- **消息历史** (`State.messages`)
- **ToolUseContext** (权限、工具列表、abort 控制器、文件状态、agentId 等)
- **transition 记录** (轻量转移日志：`next_turn`、`reactive_compact_retry`、`token_budget_continuation` 等)

**关键发现**: Claude Code 完全依赖 **LLM 自主判断** 何时停止——模型不输出 `tool_use` 就意味着完成。没有程序化的"任务完成检测"。

#### 1.4 映射到 Pre-flight 的启示

| Claude Code 的做法 | Pre-flight 的改进方向 |
|----|----|
| 完全靠 LLM 判断停止 | 不够——需要 **混合策略**：LLM 判断 + 程序化 slot 填充率检查 |
| 无 belief state | Pre-flight 必须引入显式 belief state（Contract 四区块的填充状态） |
| `needsFollowUp` 信号 | 可借鉴——用 `tool_use(suggest_sign)` 作为"澄清完成"信号 |

---

### 维度 2: Context Engineering — System Prompt 动态构建

#### 2.1 多层拼装架构

Claude Code 的 system prompt 是**多层动态拼装**的，而非静态文本：

```
层级 1: getSystemPrompt()          — 静态说明 + 动态段注册 (src/constants/prompts.ts)
层级 2: buildEffectiveSystemPrompt() — 优先级覆盖 (src/utils/systemPrompt.ts)
层级 3: appendSystemContext()       — 注入运行时上下文 (src/utils/api.ts)
层级 4: queryModel()               — 添加 attribution/fingerprint 前缀 (src/services/api/claude.ts)
```

#### 2.2 静态 vs 动态分区

一个核心设计是 **`SYSTEM_PROMPT_DYNAMIC_BOUNDARY`**——在 system prompt 数组中显式标记"静态前缀"与"动态后缀"的分界线：

```typescript
// src/constants/prompts.ts ~105-115
// Everything BEFORE this marker can use scope: 'global' (cross-org cacheable).
// Everything AFTER contains user/session-specific content.
export const SYSTEM_PROMPT_DYNAMIC_BOUNDARY =
  '__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__'
```

**静态段**（边界前）：intro、system、doing tasks、actions、using tools、tone、output efficiency  
**动态段**（边界后）通过 `systemPromptSection()` 注册：

| 段名 | 内容 | 缓存策略 |
|------|------|----------|
| `session_guidance` | 基于已启用工具的会话指引 | 会话级缓存 |
| `memory` | `loadMemoryPrompt()` 加载的 MEMORY.md | 会话级缓存 |
| `language` | 语言偏好 | 会话级缓存 |
| `output_style` | 输出风格 | 会话级缓存 |
| `mcp_instructions` | MCP 服务器说明 | 可标为 uncached |
| `token_budget` | Token 预算说明 | 条件注入 |
| `brief` | 简洁模式 | 条件注入 |

#### 2.3 CLAUDE.md 注入方式

CLAUDE.md **不是** system prompt 的一部分！它通过 **用户上下文** 注入：

```typescript
// src/utils/api.ts ~461-472
// 包装为 <system-reminder> 标签的 user 消息，prepend 到对话最前面
return [
  createUserMessage({
    content: `<system-reminder>\n# claudeMd\n${claudeMdContent}\n
    IMPORTANT: this context may or may not be relevant...\n</system-reminder>`,
    isMeta: true,
  }),
  ...messages,
]
```

#### 2.4 当前状态是否注入 system prompt？

**否。** Claude Code 的 system prompt **不包含**当前任务进度或步骤列表。进度信息依赖对话中的 tool 结果消息。这是一个有意的设计——保持 system prompt 稳定以利于缓存。

#### 2.5 cache_control 使用模式

```typescript
// src/services/api/claude.ts ~358-374
export function getCacheControl({ scope, querySource }) {
  return {
    type: 'ephemeral',
    ...(should1hCacheTTL(querySource) && { ttl: '1h' }),
    ...(scope === 'global' && { scope }),
  }
}
```

标记位置：
- **System prompt 块**：`buildSystemPromptBlocks()` 为非 null scope 的块加 `cache_control`
- **对话消息**：`addCacheBreakpoints()` 在最后一条消息加断点
- **工具定义**：`toolToAPISchema()` 支持但主路径未启用

#### 2.6 映射到 Pre-flight 的启示

| Claude Code 的做法 | Pre-flight 的改进方向 |
|----|----|
| 动态段注册机制 | 采纳——用 `{{contract_state}}`、`{{round_info}}`、`{{convergence_directive}}` 占位 |
| 静态/动态分区 | 高度适用——Pre-flight 的角色说明可全局缓存，Contract 状态按轮注入 |
| CLAUDE.md 作为 user 消息 | 项目上下文（技术栈偏好等）可以同样方式注入 |
| 不注入当前进度 | **不适用**——Pre-flight 必须注入 Contract 填充状态 |

---

### 维度 3: Tool Use 替代文本约定

#### 3.1 工具系统架构

Claude Code 的工具通过 `buildTool()` 工厂函数定义，使用 Zod schema：

```typescript
// src/Tool.ts ~362-400
export type Tool<Input, Output, P> = {
  readonly name: string
  readonly inputSchema: Input                    // Zod schema
  readonly inputJSONSchema?: ToolInputJSONSchema  // 可选 JSON Schema
  call(args, context, canUseTool, parentMessage, onProgress?): Promise<ToolResult<Output>>
  description(input, options): Promise<string>   // 动态描述
  // ...权限、并发安全性、UI 渲染等
}
```

#### 3.2 工具数量与分类

`getAllBaseTools()` 中列出约 **40+ 个工具**，按条件启用：

| 类别 | 工具 | 数量 |
|------|------|------|
| Shell/执行 | Bash, PowerShell | 1-2 |
| 文件操作 | Read, Edit, Write, NotebookEdit | 4 |
| 搜索 | Glob, Grep (或 embedded search) | 0-2 |
| Web | WebFetch, WebSearch | 2 |
| Agent/任务 | Agent, TaskOutput, TaskStop, SendMessage | 4+ |
| 计划模式 | EnterPlanMode, ExitPlanMode | 2 |
| 其他 | TodoWrite, AskUserQuestion, Brief, ToolSearch, Skill 等 | 10+ |
| MCP | 动态加载 | 不定 |

#### 3.3 设计原则

1. **最小重叠 (Minimal Overlap)**：当 embedded search 可用时，去掉独立的 Glob/Grep
2. **延迟加载 (Deferred Loading)**：`shouldDefer` 标记的工具只在 ToolSearch 命中时才暴露 schema
3. **排序稳定性**：`assembleToolPool` 对工具排序，内置工具优先于 MCP，防止缓存键抖动
4. **并发安全标记**：`isConcurrencySafe` 决定工具是否可并发执行

#### 3.4 tool_use / tool_result 配对机制

严格遵循 Anthropic Messages API 规范：

```
Assistant 消息: [
  { type: 'thinking', thinking: '...' },
  { type: 'tool_use', id: 'call_xxx', name: 'Glob', input: { pattern: '*.py' } }
]
User 消息: [
  { type: 'tool_result', tool_use_id: 'call_xxx', content: 'file1.py\nfile2.py' }
]
```

工具编排层 (`toolOrchestration.ts`) 使用 `partitionToolCalls` 将连续且 `isConcurrencySafe` 的调用批量并发执行。

#### 3.5 映射到 Pre-flight 的启示

| Claude Code 的做法 | Pre-flight 的改进方向 |
|----|----|
| Zod schema + buildTool | 采纳——用 JSON Schema 定义 Pre-flight 工具 |
| 最小重叠原则 | 高度适用——Pre-flight 只需 3-5 个专用工具 |
| tool_use 结构化输出 | **核心改进点**——用 tool_use 替代 `---CHOICES---` 文本约定 |
| 并发安全标记 | Pre-flight 工具均为串行，暂不需要 |

---

### 维度 4: Sub-agent 架构与上下文隔离

#### 4.1 何时 Spawn Sub-agent

Claude Code 中 **没有程序化的调度器**——是否调用 `Agent` 工具由 **主模型根据 system prompt 中的指引自主决定**：

```
// system prompt 中的指引 (来自逆向工程)
VERY IMPORTANT: When exploring the codebase to gather context or to answer
a question that is not a needle query for a specific file/class/function,
it is CRITICAL that you use the Task tool with subagent_type=Explore
instead of running search commands directly.
```

路由逻辑在 `AgentTool.tsx` 的 `call()` 中：
- `team_name + name` → 队友 (swarm)
- `subagent_type` 指定 → 对应内置 agent
- 未指定 + FORK_SUBAGENT 开启 → fork 路径
- 默认 → general-purpose agent

#### 4.2 Sub-agent 与主 Agent 的差异

| 维度 | 主 Agent | Sub-agent |
|------|----------|-----------|
| System prompt | 完整 `getSystemPrompt()` | Agent 定义的 `getSystemPrompt()` 或继承父级 |
| 工具列表 | 全量 `assembleToolPool()` | `filterToolsForAgent()` 过滤后的子集 |
| 禁用工具 | 无 | `AskUserQuestion`、`TaskOutput`、`TaskStop`、`EnterPlanMode` 等 |
| Thinking | 启用 | 默认禁用 (节省 output token) |
| 模型 | 主模型 | 默认继承，可通过 `model` 参数或 `CLAUDE_CODE_SUBAGENT_MODEL` 覆盖 |

```typescript
// src/constants/tools.ts ~36-55
export const ALL_AGENT_DISALLOWED_TOOLS = new Set([
  TASK_OUTPUT_TOOL_NAME,
  EXIT_PLAN_MODE_V2_TOOL_NAME,
  ENTER_PLAN_MODE_TOOL_NAME,
  AGENT_TOOL_NAME,  // 非 ant 环境下禁止嵌套
  ASK_USER_QUESTION_TOOL_NAME,
  TASK_STOP_TOOL_NAME,
])
```

#### 4.3 上下文隔离机制

`createSubagentContext()` 创建隔离的 `ToolUseContext`：

```typescript
// src/utils/forkedAgent.ts ~306-461
export function createSubagentContext(parentContext, overrides?) {
  return {
    readFileState: cloneFileStateCache(parentContext.readFileState), // 克隆
    setAppState: () => {},  // no-op（不影响父级 UI）
    messages: overrides?.messages ?? parentContext.messages,
    agentId: createAgentId(),  // 新 ID
    queryTracking: {
      chainId: randomUUID(),
      depth: (parentContext.queryTracking?.depth ?? -1) + 1,  // 深度+1
    },
    // ... 独立的 AbortController（链接到父级，父 abort 传播）
  }
}
```

#### 4.4 输出如何传回主 Agent

`finalizeAgentTool()` 提取子 agent 最后一条 assistant 消息的文本块，作为 `tool_result` 返回：

```typescript
// src/tools/AgentTool/agentToolUtils.ts ~276-356
export function finalizeAgentTool(agentMessages, agentId, metadata) {
  const lastAssistantMessage = getLastAssistantMessage(agentMessages)
  let content = lastAssistantMessage.message.content.filter(_ => _.type === 'text')
  return {
    agentId, agentType, content,
    totalDurationMs, totalTokens, totalToolUseCount, usage,
  }
}
```

#### 4.5 "80% of success came from token volume advantage"

来自 Anthropic 官方博客的解释：

> 每个 sub-agent 可能消耗数万 token 进行深度探索，但只返回 **1,000-2,000 token 的精炼摘要**。这实现了清晰的关注点分离——详细的搜索上下文被隔离在 sub-agent 中，主 agent 只需关注综合与分析。

#### 4.6 映射到 Pre-flight 的启示

| Claude Code 的做法 | Pre-flight 的改进方向 |
|----|----|
| Prompt 驱动的 spawn 决策 | 中度适用——Pre-flight 的三种模式可由主 agent 自主切换 |
| 工具子集过滤 | 采纳——不同澄清模式应有不同的工具集 |
| 输出精炼返回 | 高度适用——子模式输出应精炼后写入 Contract |
| 上下文隔离 | 中度适用——Pre-flight 的子模式可共享 Contract 状态但隔离对话 |
| Fork 继承父 prompt (cache) | 技术细节——DashScope 不支持 prompt caching，优先级低 |

---

### 维度 5: Compaction 与上下文压缩

#### 5.1 触发时机

自动压缩由 `shouldAutoCompact()` 判断：

```typescript
// src/services/compact/autoCompact.ts ~72-91
export function getAutoCompactThreshold(model: string): number {
  const effectiveContextWindow = getEffectiveContextWindowSize(model)
  return effectiveContextWindow - AUTOCOMPACT_BUFFER_TOKENS  // 缓冲 13k tokens
}
```

触发条件：`估算 token 数 >= 有效上下文窗口 - 13k`

递归保护：`querySource === 'session_memory' || 'compact'` 时不触发（防死锁）。

连续失败保护：超过 **3 次** compact 失败后熔断，不再尝试。

#### 5.2 压缩 Prompt（完整）

压缩提示词结构为 `NO_TOOLS_PREAMBLE + BASE_COMPACT_PROMPT + 可选自定义指令 + NO_TOOLS_TRAILER`：

```
CRITICAL: Respond with TEXT ONLY. Do NOT call any tools.

Your task is to create a detailed summary of the conversation so far...

Your summary should include the following sections:
1. Primary Request and Intent
2. Key Technical Concepts
3. Files and Code Sections
4. Errors and fixes
5. Problem Solving
6. All user messages (非 tool result 的用户消息)
7. Pending Tasks
8. Current Work
9. Optional Next Step
```

#### 5.3 Tool Result Clearing (Micro-compact)

对 `COMPACTABLE_TOOLS` 集合中的工具结果进行清理：

```typescript
// src/services/compact/microCompact.ts ~40-50
const COMPACTABLE_TOOLS = new Set([
  'Read', 'Bash', 'Grep', 'Glob',
  'WebSearch', 'WebFetch', 'Edit', 'Write'
])
```

策略：时间间隔超过阈值时，将**除最近 N 条外**的 tool_result 内容替换为：
```
[Old tool result content cleared]
```

保留 `tool_use` 调用记录（名称、参数），只清理 `tool_result` 的文本内容。

#### 5.4 压缩后的上下文

```typescript
// src/services/compact/compact.ts ~325-337
export function buildPostCompactMessages(result: CompactionResult): Message[] {
  return [
    result.boundaryMarker,       // 压缩边界标记
    ...result.summaryMessages,   // summary 用户消息
    ...(result.messagesToKeep ?? []),  // 可选保留段
    ...result.attachments,       // 最近文件状态恢复
    ...result.hookResults,       // Hook 结果
  ]
}
```

Summary 用户消息的格式：
```
This session is being continued from a previous conversation that ran
out of context. The summary below covers the earlier portion...

[formatted summary]

If you need specific details from before compaction, read the full
transcript at: [path]
```

#### 5.5 Fallback 机制

1. **截断头部重试**：summary 请求返回 PROMPT_TOO_LONG 时截断早期消息重试
2. **Transcript 路径**：压缩后消息中引导从磁盘读原文
3. **双路径生成**：先 fork + cache 共享，失败再 streaming
4. **连续失败熔断**：3 次失败后停止尝试
5. **PreCompact hook**：可附加说明或阻断压缩

#### 5.6 映射到 Pre-flight 的启示

| Claude Code 的做法 | Pre-flight 的改进方向 |
|----|----|
| Token 阈值触发 | 采纳——但 Pre-flight 可用更简单的 **轮次计数** 触发（如 >8 轮） |
| 9 段 summary 结构 | 改造——Pre-flight 的 summary 应围绕 Contract 四区块 |
| Tool result clearing | 高度适用——清理选择题的详细选项说明，保留选择结果 |
| Transcript 回退 | 采纳——保留完整对话日志供按需检索 |

---

### 维度 6: 结构化笔记（Memory）

#### 6.1 分层记忆体系

Claude Code 有 **6 层记忆**：

| 层级 | 来源 | 注入方式 | 持久性 |
|------|------|----------|--------|
| 1. 组织级 | 托管设置 | system prompt | 跨项目 |
| 2. 用户级 | `~/.claude/CLAUDE.md` | user 消息 `<system-reminder>` | 跨项目 |
| 3. 项目级 | 项目 `CLAUDE.md`、`.claude/rules/*.md` | user 消息 `<system-reminder>` | 跨会话 |
| 4. 自动记忆 | `~/.claude/projects/<path>/memory/` | system prompt `memory` 段 | 跨会话 |
| 5. Session Memory | 会话级 markdown 笔记 | 后台 fork agent 更新 | 会话内 |
| 6. Compaction Summary | 压缩生成的摘要 | 替换消息历史 | 会话内 |

#### 6.2 自动记忆抽取

在每轮完整结束时（模型无 tool calls 的最终回复），通过 `handleStopHooks` 异步触发：

```typescript
// src/services/extractMemories/extractMemories.ts ~1-14
// 在 query 结束时触发；使用 forked agent pattern——
// 共享父级 prompt cache 的完美 fork。
```

关键设计：若主 agent 已显式 Write/Edit 到 auto-memory 路径，后台抽取 **跳过**（避免重复）。

#### 6.3 存储格式

**Markdown 文件** + **YAML frontmatter**：

```yaml
---
type: architecture_decision  # 类型枚举
description: "选择 PostgreSQL 作为主数据库"
---
## 决策
基于团队经验和事务需求选择 PostgreSQL...
```

扫描时通过 `memoryScan.ts` 解析 frontmatter 的 `type` 和 `description`。

#### 6.4 注入策略

- **System prompt**：`loadMemoryPrompt()` 加载 MEMORY.md 入口和机制说明
- **User 消息**：全量 `getClaudeMds()` 内容通过 `<system-reminder>` 预挂
- **按需搜索**：feature 开启时指导 agent 在记忆目录上 grep
- **无向量库**：不使用 embedding/RAG，全靠文件系统 + grep

#### 6.5 映射到 Pre-flight 的启示

| Claude Code 的做法 | Pre-flight 的改进方向 |
|----|----|
| Contract 即结构化笔记 | 高度适用——Contract 本身就是 Pre-flight 的 "MEMORY.md" |
| Markdown + frontmatter | 可借鉴——Contract 条目可用类似格式持久化 |
| 后台自动抽取 | 中度适用——每轮自动更新 Contract JSON 而非等用户操作 |
| 全量注入 + "不相关则忽略" | 采纳——将 Contract 当前状态全量注入但标记为参考 |

---

### 维度 7: Prompt Caching 与性能优化

#### 7.1 Dummy Request 缓存预热

George Sung 的逆向分析发现：Claude Code 在会话开始时发送 `max_tokens: 1` 的请求：

```json
{
  "max_tokens": 1,
  "messages": [{ "role": "user", "content": "count" }],
  "tools": [/* 完整工具列表 */]
}
```

**目的**：将 system prompt + 工具定义写入 prompt cache，后续请求直接命中缓存。延迟被与 metadata 生成请求的并行执行所"隐藏"。

**但**源码中 `sessionStart.ts` 明确标注 `// do not add ANY "warmup" logic`——说明当前版本可能已移除或改为其他方式。

#### 7.2 双模型策略

Claude Code 使用 **轻量模型 + 重量模型** 分工：

| 任务 | 模型 | 说明 |
|------|------|------|
| 生成对话标题 | `getSmallFastModel()` (如 Haiku) | 低成本元数据 |
| 生成话题分类 | `getSmallFastModel()` | 低成本元数据 |
| API Key 验证 | `getSmallFastModel()` + `max_tokens: 1` | 最低成本探测 |
| 主对话循环 | 主模型 (如 Opus/Sonnet) | 高质量推理 |
| Sub-agent (默认) | 继承主模型 | 可通过 env 覆盖 |
| Sub-agent (Explore) | 可配置为轻量模型 | 降低搜索成本 |
| Compaction | fork 共享 cache 或 streaming | 复用缓存 |

#### 7.3 Beta Header 粘滞策略

为避免打碎 50-70K token 级别的缓存：
- Auto mode、fast mode、cache editing 等 beta 标记在会话中 **固定不变**
- 工具列表排序稳定
- MCP 工具放在内置工具后面

#### 7.4 主流模型 Prompt Caching 支持情况

> **更正说明 (2026-04-08)**：经核实 [DashScope 官方文档](https://www.alibabacloud.com/help/en/model-studio/context-cache)，qwen3.5-plus **支持** prompt caching，且显式缓存语法与 Anthropic 的 `cache_control: { type: "ephemeral" }` **完全兼容**。

| 提供商 | 支持？ | 显式 `cache_control` | 隐式自动 | 命中折扣 | 最低 token |
|--------|--------|---------------------|---------|---------|-----------|
| **Anthropic** | ✅ | ✅ | ❌ | 命中 -90% | 1024-4096 |
| **OpenAI** | ✅ | ❌ | ✅（全自动） | 命中 -50% | 1024 |
| **DashScope (通义)** | ✅ | ✅ **兼容 Anthropic 语法** | ✅（默认开启） | 显式 -90% / 隐式 -80% | 显式 1024 / 隐式 256 |
| **DeepSeek** | ✅ | ❌ | ✅（全自动，磁盘级） | 命中 -90% | 64 粒度 |
| **MiniMax** | ✅ | ✅ **兼容 Anthropic 语法** | ✅ | 命中 -90% | 未明确 |
| **Google Gemini** | ✅ | ✅（创建 cache 对象） | ✅（2.5+ 模型） | 按 TTL | 1024-4096 |
| **智谱 GLM** | ⚠️ | 自建推理可用 (vLLM) | 官方 API 未见文档 | — | — |

DashScope 显式缓存的关键参数：
- **创建成本**: 标准输入价的 125%
- **命中成本**: 标准输入价的 **10%**（1 折）
- **有效期**: 5 分钟（每次命中自动续期）
- **每请求最多标记数**: 4 个
- **可缓存内容**: system / user / assistant / tool 消息均支持
- **tools 参数**: 在 messages 中添加 cache 标记时，请求中的 tools 定义也会被一并缓存

#### 7.5 映射到 Pre-flight 的启示

| Claude Code 的做法 | Pre-flight 的改进方向 |
|----|----|
| 双模型策略 | 高度适用——用轻量模型做 slot 分类/选项生成，重量模型做深度推理 |
| Cache warmup (`max_tokens:1`) | **高度适用**——qwen3.5-plus 支持显式缓存，可在首轮对话前用 dummy 请求预热 system prompt + tools 的 KV Cache |
| `SYSTEM_PROMPT_DYNAMIC_BOUNDARY` 分区 | **高度适用**——将 Pre-flight 的静态角色说明与动态 Contract 状态分开，静态部分可跨请求缓存 |
| `addCacheBreakpoints()` 滚动断点 | **直接可用**——DashScope 语法兼容，每轮在最后一条消息标 `cache_control` 即可 |
| 排序稳定性 | **高度适用**——工具定义顺序和 system prompt 前缀保持稳定，避免打碎缓存 |

---

## 第二部分：关键设计模式提取

### 模式 1: Tool-as-Structure（工具即结构）

**在 Claude Code 中的实现**: 所有 agent 与外部世界的交互通过 tool_use 完成，包括文件操作、搜索、甚至切换模式（`EnterPlanMode`）。tool_use 的 JSON Schema 保证了输入/输出的结构化。

**对 Miragenty Pre-flight 的适用性**: ⭐⭐⭐ **高**

**推荐实现方案**:

```json
{
  "tools": [
    {
      "name": "present_choices",
      "description": "向用户展示一组选项供确认或选择，用于需求澄清",
      "input_schema": {
        "type": "object",
        "properties": {
          "question": { "type": "string", "description": "澄清问题" },
          "dimension": {
            "type": "string",
            "enum": ["scope", "constraints", "exclusions", "assumptions"],
            "description": "问题所属的 Contract 区块"
          },
          "choices": {
            "type": "array",
            "items": {
              "type": "object",
              "properties": {
                "id": { "type": "string" },
                "label": { "type": "string" },
                "description": { "type": "string" },
                "impact": { "type": "string", "description": "选择此项的影响" }
              },
              "required": ["id", "label"]
            },
            "minItems": 2
          },
          "allow_multiple": { "type": "boolean", "default": false },
          "allow_custom": { "type": "boolean", "default": true }
        },
        "required": ["question", "dimension", "choices"]
      }
    },
    {
      "name": "add_contract_item",
      "description": "向 Mission Contract 添加一个已确认的条目",
      "input_schema": {
        "type": "object",
        "properties": {
          "section": {
            "type": "string",
            "enum": ["scope", "constraints", "exclusions", "assumptions"]
          },
          "item": { "type": "string", "description": "条目内容" },
          "confidence": {
            "type": "string",
            "enum": ["confirmed", "tentative", "inferred"],
            "description": "确认程度"
          },
          "source_round": { "type": "integer", "description": "来源轮次" },
          "rationale": { "type": "string", "description": "为何做出此决策" }
        },
        "required": ["section", "item", "confidence"]
      }
    },
    {
      "name": "update_contract_item",
      "description": "修改已有的 Contract 条目（基于后续澄清）",
      "input_schema": {
        "type": "object",
        "properties": {
          "item_id": { "type": "string" },
          "new_content": { "type": "string" },
          "new_confidence": { "type": "string", "enum": ["confirmed", "tentative", "inferred"] },
          "reason": { "type": "string" }
        },
        "required": ["item_id", "new_content"]
      }
    },
    {
      "name": "suggest_sign",
      "description": "当 Agent 认为澄清已充分时，建议签署 Contract",
      "input_schema": {
        "type": "object",
        "properties": {
          "readiness_assessment": {
            "type": "object",
            "properties": {
              "scope_completeness": { "type": "number", "minimum": 0, "maximum": 1 },
              "constraints_completeness": { "type": "number", "minimum": 0, "maximum": 1 },
              "risk_coverage": { "type": "number", "minimum": 0, "maximum": 1 }
            }
          },
          "remaining_concerns": {
            "type": "array",
            "items": { "type": "string" }
          },
          "summary": { "type": "string" }
        },
        "required": ["readiness_assessment", "summary"]
      }
    },
    {
      "name": "switch_clarification_mode",
      "description": "切换澄清策略",
      "input_schema": {
        "type": "object",
        "properties": {
          "mode": {
            "type": "string",
            "enum": ["scenario_walkthrough", "devils_advocate", "risk_tagging"],
            "description": "场景走查 / 魔鬼代言人 / 风险标记"
          },
          "reason": { "type": "string" }
        },
        "required": ["mode"]
      }
    }
  ]
}
```

---

### 模式 2: Dynamic System Prompt Assembly（动态系统提示拼装）

**在 Claude Code 中的实现**: 使用 `systemPromptSection()` 注册机制 + `SYSTEM_PROMPT_DYNAMIC_BOUNDARY` 分界线，实现静态前缀与动态后缀的分离。

**对 Miragenty Pre-flight 的适用性**: ⭐⭐⭐ **高**

**推荐实现方案**: 见第三部分的 System Prompt 模板。

---

### 模式 3: Compaction with Structured Summary（结构化摘要压缩）

**在 Claude Code 中的实现**: 9 段固定结构的压缩 prompt，要求先 `<analysis>` 后 `<summary>`，strip analysis 后保留 summary。

**对 Miragenty Pre-flight 的适用性**: ⭐⭐⭐ **高**

**推荐实现方案**:

```typescript
function getPreflightCompactPrompt(contractState: ContractState): string {
  return `请将以上 Pre-flight 澄清对话压缩为结构化摘要。

当前 Contract 状态：
${JSON.stringify(contractState, null, 2)}

请按以下结构输出：

<analysis>
[分析过程——哪些讨论已经稳定，哪些仍在变化]
</analysis>

<summary>
1. 用户原始需求：[原文引用]
2. 已确认条目摘要：
   - Scope: [列表]
   - Constraints: [列表]
   - Exclusions: [列表]
   - Assumptions: [列表]
3. 关键决策及理由：[决策 → 理由 列表]
4. 仍待澄清的问题：[列表]
5. 用户偏好与风格：[观察到的沟通偏好]
</summary>`
}
```

---

### 模式 4: Sub-agent Context Isolation（子代理上下文隔离）

**在 Claude Code 中的实现**: `createSubagentContext()` 克隆文件状态、隔离消息、独立 AbortController、缩减工具列表。

**对 Miragenty Pre-flight 的适用性**: ⭐⭐ **中**

Pre-flight 的三种澄清模式（场景走查 / 魔鬼代言人 / 风险标记）**不建议** 拆为独立 sub-agent，因为：
1. 它们共享同一份 Contract 状态
2. 用户期望连续对话体验
3. DashScope 模型切换的延迟成本高

**推荐方案**: 用 **模式切换工具 + system prompt 动态段** 替代 sub-agent：

```typescript
const CLARIFICATION_MODE_PROMPTS = {
  scenario_walkthrough: `你当前处于【场景走查】模式。请通过具体使用场景引导用户思考需求边界。
    重点关注: 典型用户旅程、边界场景、异常流程。`,
  devils_advocate: `你当前处于【魔鬼代言人】模式。请对用户的假设提出质疑和挑战。
    重点关注: 隐含假设、技术风险、遗漏需求。`,
  risk_tagging: `你当前处于【风险标记】模式。请识别并标记技术和业务风险。
    重点关注: 性能瓶颈、安全风险、集成复杂度、时间风险。`
}
```

---

### 模式 5: Micro-compact / Tool Result Clearing（工具结果清理）

**在 Claude Code 中的实现**: 对可压缩工具（Read, Bash, Grep 等）的 tool_result 内容替换为 `[Old tool result content cleared]`，保留调用记录。

**对 Miragenty Pre-flight 的适用性**: ⭐⭐⭐ **高**

**推荐实现方案**:

```typescript
function microCompactPreflightMessages(messages: Message[], keepRecent: number = 3): Message[] {
  const choiceToolResults = findToolResults(messages, 'present_choices')
  const toKeep = new Set(choiceToolResults.slice(-keepRecent).map(r => r.tool_use_id))
  
  return messages.map(msg => {
    if (msg.type === 'tool_result' && msg.name === 'present_choices' && !toKeep.has(msg.tool_use_id)) {
      return {
        ...msg,
        // 保留用户选择结果，清理详细选项描述
        content: `[选项详情已压缩] 用户选择: ${msg.userSelection}`
      }
    }
    return msg
  })
}
```

---

### 模式 6: Layered Memory Injection（分层记忆注入）

**在 Claude Code 中的实现**: 6 层记忆按优先级加载，CLAUDE.md 作为 user 消息 `<system-reminder>` 注入，auto-memory 通过 system prompt 段注入。

**对 Miragenty Pre-flight 的适用性**: ⭐⭐ **中**

**推荐实现方案**:

```typescript
// Pre-flight 的两层记忆
const preflightMemory = {
  // 层1: 项目上下文（类似 CLAUDE.md）——全量注入
  projectContext: {
    techStack: "React 19 + Tauri 2.0",
    teamSize: 3,
    previousDecisions: [...],
    injectionPoint: 'user_message_system_reminder'  // <system-reminder>
  },
  // 层2: Contract 状态（类似 Session Memory）——每轮动态注入
  contractState: {
    scope: [...confirmedItems],
    constraints: [...confirmedItems],
    exclusions: [...confirmedItems],
    assumptions: [...confirmedItems],
    injectionPoint: 'system_prompt_dynamic_section'
  }
}
```

---

### 模式 7: Convergence via Belief State（信念状态驱动收敛）

**在 Claude Code 中的实现**: **不存在**——这是 Claude Code 缺少但 Pre-flight 必须有的模式。

**对 Miragenty Pre-flight 的适用性**: ⭐⭐⭐ **高（必要创新）**

**推荐实现方案**:

```typescript
interface PreflightBeliefState {
  round: number
  maxRounds: number  // 如 12
  
  slots: {
    // 每个 slot 有 filled/pending/skipped 状态
    primaryGoal: SlotState
    targetUsers: SlotState
    techConstraints: SlotState
    securityRequirements: SlotState
    performanceTargets: SlotState
    integrationPoints: SlotState
    outOfScope: SlotState
    riskAssumptions: SlotState
    // ... 可扩展
  }
  
  convergenceScore: number  // 0-1，基于 confirmed 条目数 / 总 slot 数
  
  // 收敛策略
  phase: 'exploring' | 'narrowing' | 'confirming' | 'ready_to_sign'
}

// 收敛指令注入
function getConvergenceDirective(state: PreflightBeliefState): string {
  if (state.round >= state.maxRounds * 0.8) {
    return `⚠️ 已接近轮次上限 (${state.round}/${state.maxRounds})。
请优先确认剩余待填 slot，避免引入新话题。
当前未填 slot: ${getUnfilledSlots(state).join(', ')}`
  }
  if (state.convergenceScore > 0.85) {
    return `✅ 大部分需求已澄清 (${Math.round(state.convergenceScore * 100)}%)。
考虑调用 suggest_sign 建议签署。`
  }
  return `当前澄清进度: ${Math.round(state.convergenceScore * 100)}%
重点关注: ${getHighPrioritySlots(state).join(', ')}`
}
```

---

### 模式 8: Needles-Follow-Up Signal（工具调用作为继续信号）

**在 Claude Code 中的实现**: `needsFollowUp` 布尔标志——有 tool_use 就继续，没有就结束。

**对 Miragenty Pre-flight 的适用性**: ⭐⭐⭐ **高**

**推荐实现方案**:

```typescript
// Pre-flight 的 loop
async function preflightLoop(initialMessage: string, contractState: ContractState) {
  while (true) {
    const response = await callLLM(buildMessages(contractState))
    
    for (const block of response.content) {
      if (block.type === 'tool_use') {
        switch (block.name) {
          case 'present_choices':
            const userChoice = await presentChoicesToUser(block.input)
            appendToolResult(block.id, userChoice)
            break
          case 'add_contract_item':
            contractState = addItem(contractState, block.input)
            appendToolResult(block.id, { success: true, contractState })
            break
          case 'suggest_sign':
            const userDecision = await askUserToSign(block.input)
            if (userDecision.signed) return contractState
            appendToolResult(block.id, { signed: false, reason: userDecision.reason })
            break
        }
        needsFollowUp = true
      }
    }
    
    if (!needsFollowUp) {
      // LLM 没有调用工具——视为"等待用户输入"
      const userMessage = await waitForUserInput()
      appendUserMessage(userMessage)
    }
  }
}
```

---

## 第三部分：Pre-flight 优化方案

### 3.1 改造后的 System Prompt 模板

```
你是 Miragenty 的 Pre-flight Planner Agent，专门负责通过多轮对话澄清用户的高层需求，
构建 Mission Contract。

# 你的目标
将用户的模糊需求转化为结构化、完整、可执行的 Mission Contract，
包含 Scope / Constraints / Exclusions / Assumptions 四个区块。

# 当前状态
{{contract_state_json}}

# 轮次信息
当前是第 {{current_round}} 轮，最多 {{max_rounds}} 轮。
{{convergence_directive}}

# 澄清模式
当前模式: {{clarification_mode}}
{{mode_specific_instructions}}

# 对话策略
- 每轮聚焦 1-2 个维度，不要一次问太多
- 优先使用 present_choices 工具提供结构化选项
- 用户确认后立即用 add_contract_item 写入 Contract
- 随着澄清深入，减少开放式问题，增加确认式问题
- 当收敛分数 > 85% 或重要 slot 均已填充时，考虑 suggest_sign

# 信念状态追踪
未填充的 slot: {{unfilled_slots}}
已确认条目数: {{confirmed_count}}
待确认(tentative)条目数: {{tentative_count}}
收敛分数: {{convergence_score}}%

# 工具使用规范
- present_choices: 需要用户做选择时使用，必须指定 dimension
- add_contract_item: 用户确认后写入，必须标注 confidence
- update_contract_item: 后续讨论推翻了之前的假设时使用
- suggest_sign: 仅在收敛分数 > 80% 时使用
- switch_clarification_mode: 当前模式效率低下时切换

# 输出格式
永远不要使用 ---CHOICES--- 分隔符。所有结构化输出通过 tool_use 完成。
在 tool_use 之外的文本用于向用户解释推理过程和上下文。
```

### 3.2 Compaction 策略

```typescript
interface PreflightCompactionConfig {
  // 触发条件（二选一）
  triggerConditions: {
    roundThreshold: 8,           // 超过 8 轮触发
    tokenThreshold: 0.7,         // 占用 70% context window 触发
  }
  
  // 压缩策略
  strategy: {
    // 永远保留
    alwaysKeep: [
      'contract_state',          // Contract 当前状态（从 belief state 重建）
      'last_3_rounds',           // 最近 3 轮完整对话
      'user_original_request',   // 用户原始需求
    ],
    // 有条件保留
    keepIfRelevant: [
      'decision_rationales',     // 关键决策的理由
      'rejected_options',        // 被否决的选项（防止重复提问）
    ],
    // 可安全丢弃
    discard: [
      'choice_option_details',   // 选项的详细描述
      'intermediate_reasoning',  // 中间推理过程
      'greetings_and_meta',      // 寒暄和元对话
    ]
  }
  
  // Micro-compact (不触发完整压缩时)
  microCompact: {
    clearChoiceDetailsAfterRounds: 3,  // 3 轮后清理选项详情
    keepSelectionResult: true,          // 保留选择结果
  }
}
```

### 3.3 对话状态追踪数据结构

```typescript
interface PreflightState {
  // === Session 元信息 ===
  sessionId: string
  startedAt: Date
  
  // === 对话管理 ===
  currentRound: number
  maxRounds: number
  clarificationMode: 'scenario_walkthrough' | 'devils_advocate' | 'risk_tagging'
  
  // === Mission Contract ===
  contract: {
    scope: ContractItem[]
    constraints: ContractItem[]
    exclusions: ContractItem[]
    assumptions: ContractItem[]
  }
  
  // === Belief State ===
  beliefState: {
    slots: Record<string, {
      status: 'unfilled' | 'tentative' | 'confirmed' | 'skipped'
      value: string | null
      confirmedAtRound: number | null
      lastModifiedAtRound: number | null
    }>
    convergenceScore: number  // 自动计算
    phase: 'exploring' | 'narrowing' | 'confirming' | 'ready_to_sign'
  }
  
  // === 对话历史管理 ===
  messages: Message[]  // 完整历史
  compactedAt: number | null  // 上次压缩的轮次
  compactionSummary: string | null  // 压缩摘要
  
  // === 决策日志 ===
  decisionLog: {
    round: number
    decision: string
    rationale: string
    alternatives: string[]
  }[]
}

interface ContractItem {
  id: string
  section: 'scope' | 'constraints' | 'exclusions' | 'assumptions'
  content: string
  confidence: 'confirmed' | 'tentative' | 'inferred'
  sourceRound: number
  rationale?: string
  modifiedHistory?: { round: number; oldContent: string; reason: string }[]
}
```

### 3.4 实施优先级排序

| 优先级 | 改进项 | 预期效果 | 实施复杂度 | 依赖 |
|--------|--------|----------|------------|------|
| **P0** | Tool-as-Structure（用 tool_use 替代文本约定） | 解析可靠性从 ~70% 提升到 ~99% | 中 | 无 |
| **P0** | Belief State 数据结构 | 使收敛可量化、可追踪 | 低 | 无 |
| **P1** | Dynamic System Prompt（注入 Contract 状态 + 收敛指令） | 每轮 LLM 都知道当前状态和下一步方向 | 中 | P0 |
| **P1** | Convergence Mechanism（收敛机制） | 解决"不知何时停止"问题 | 低 | P0, P1 |
| **P1** | Prompt Caching（显式缓存标记） | system prompt + tools 缓存命中后成本降低 ~78%，延迟显著降低 | **低** | 无（qwen3.5-plus 原生兼容 Anthropic `cache_control` 语法） |
| **P2** | Micro-compact（选项详情清理） | 降低 30-50% token 消耗 | 低 | P0 |
| **P2** | 决策日志（Decision Log） | 支持"为什么做出此决策"的可追溯性 | 低 | P0 |
| **P3** | Full Compaction（完整压缩） | 支持超长 Pre-flight 会话（>12 轮） | 中 | P1, P2 |
| **P3** | 澄清模式切换 | 提升澄清效率和覆盖面 | 低 | P1 |
| **P4** | 双模型策略 | 降低成本——轻量模型做 slot 分类 | 高 | 需要额外模型接入 |

---

## 第四部分：风险与取舍

### 4.1 Tool Use 替代文本约定

| 维度 | 收益 | 风险 |
|------|------|------|
| **可靠性** | 解析可靠性大幅提升 | 非 Anthropic 模型（如 qwen）的 tool_use 遵从度可能不如 Claude |
| **Token 成本** | — | 工具 schema 定义增加 ~500-1000 token 固定消耗 |
| **灵活性** | 强类型保证 | 工具 schema 变更需要同步更新客户端解析 |
| **调试** | 结构化日志更清晰 | 需要构建 tool_result 的 mock 测试框架 |

**缓解措施**: 
- 对 qwen 模型做充分的 tool_use 遵从度测试
- 保留 text fallback 解析作为降级方案
- 工具定义使用版本号管理

### 4.2 Dynamic System Prompt

| 维度 | 收益 | 风险 |
|------|------|------|
| **LLM 感知力** | 每轮都知道当前状态 | Contract JSON 增加 system prompt 长度 |
| **收敛效率** | 收敛指令引导 LLM 行为 | 过强的指令可能限制 LLM 灵活性 |
| **Token 成本** | — | Contract 状态随对话增长，增加输入 token |

**缓解措施**: 
- Contract 状态用紧凑 JSON 而非自然语言描述
- 设置 Contract 注入的最大 token 上限（如 2000 token）
- 收敛指令根据阶段动态调整强度

### 4.3 Compaction 信息丢失

| 维度 | 收益 | 风险 |
|------|------|------|
| **Token 节省** | 长对话成本降低 50-70% | 可能丢失微妙的上下文信息 |
| **质量** | 避免 context rot | 压缩本身消耗一次 LLM 调用 |
| **连续性** | 支持超长会话 | 压缩质量取决于 LLM 能力 |

**缓解措施**: 
- Contract 状态独立于消息历史维护（不依赖压缩保留）
- 保留完整对话日志供按需检索
- 压缩后注入 `[完整对话日志可在此查看: {path}]`
- 先实现 micro-compact，延迟实现 full compaction

### 4.4 Belief State 维护

| 维度 | 收益 | 风险 |
|------|------|------|
| **收敛可见性** | 量化澄清进度 | Slot 定义可能不适用于所有项目类型 |
| **自动化** | 程序化判断何时签署 | 过于机械的 slot 检查可能错过自然收敛 |
| **可追溯性** | 记录决策路径 | 增加状态同步的复杂度 |

**缓解措施**: 
- Slot 列表可配置，支持项目级自定义
- Belief state 仅作为辅助信号，最终签署权在用户
- LLM 可通过 `add_contract_item(confidence='inferred')` 推断填充

### 4.5 模型兼容性

| 维度 | 收益 | 风险 |
|------|------|------|
| **成本** | qwen3.5-plus 成本远低于 Claude | tool_use 行为可能与 Claude 有差异 |
| **可用性** | DashScope 无需梯子 | thinking tokens 不可用 |
| **Prompt Caching** | qwen3.5-plus 支持显式缓存（兼容 Anthropic `cache_control` 语法），命中成本仅为标准价的 10% | 缓存有效期 5 分钟，Pre-flight 轮次间隔超过 5 分钟会失效 |
| **质量** | — | 复杂推理和 schema 遵从可能不如 Claude |

**缓解措施**: 
- 工具 schema 尽量简单，减少 `enum` 和嵌套
- 添加 `description` 字段的详细说明
- 在 system prompt 中强调工具使用格式
- 利用显式缓存降低 agent loop 中重复 system prompt 的成本
- 考虑关键场景 fallback 到 Claude API

---

## 第五部分：参考文献索引

### 一手来源（Anthropic 官方）

| # | 来源 | 关键结论 |
|---|------|----------|
| 1 | [Effective Context Engineering](https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents) (2025.09) | Context 是稀缺资源；compaction + structured note-taking + sub-agent 是三大长会话策略；tool result clearing 是最安全的轻量压缩 |
| 2 | [Building Effective Agents](https://www.anthropic.com/research/building-effective-agents) (2024.12) | 最成功的实现用简单可组合模式；agent = LLM 在 loop 中使用 tools；工具设计应投入与 HCI 同等精力 |
| 3 | [Multi-agent Research System](https://www.anthropic.com/engineering/multi-agent-research-system) | Sub-agent 的核心收益是 token volume advantage——每个 sub-agent 消耗数万 token 但只返回 1-2k 精炼摘要 |
| 4 | [Writing Tools for Agents](https://www.anthropic.com/engineering/writing-tools-for-agents) | 工具应自包含、错误健壮、描述清晰；参数名应消除歧义；工具集应最小且无重叠 |

### 逆向工程分析

| # | 来源 | 关键结论 |
|---|------|----------|
| 5 | [George Sung: Tracing Claude Code's LLM Traffic](https://medium.com/@georgesung/tracing-claude-codes-llm-traffic-agentic-loop-sub-agents-tool-use-prompts-7796941806f5) (2026.01) | 确认双模型策略；cache warmup 用 `max_tokens:1`；CLAUDE.md 通过 `<system-reminder>` user 消息注入；sub-agent 用轻量模型 |
| 6 | [Vikash Rungta: Claude Code Architecture](https://vrungta.substack.com/p/claude-code-architecture-reverse) (2026.02) | TAOR loop (Think-Act-Observe-Repeat)；5 个设计支柱；6 层记忆；Capability Primitives > 专用集成 |
| 7 | [Penligent: Inside Claude Code](https://www.penligent.ai/hackinglabs/es/inside-claude-code-the-architecture-behind-tools-memory-hooks-and-mcp/) | Tools、Memory、Hooks、MCP 的内部实现细节 |
| 8 | [Kotrotsos: Context Management](https://kotrotsos.medium.com/claude-code-internals-part-13-context-management-ffa3f4a0f6b4) | 上下文管理内部细节，auto-compaction 策略 |

### 源码仓库分析（本次调研）

| 关键文件 | 分析维度 |
|----------|----------|
| `src/query.ts` | Agentic loop、State 类型、needsFollowUp 信号、compaction 集成 |
| `src/constants/prompts.ts` | System prompt 静态/动态分区、DYNAMIC_BOUNDARY |
| `src/tools.ts` + `src/Tool.ts` | 工具注册、buildTool 工厂、getAllBaseTools |
| `src/tools/AgentTool/` | Sub-agent spawn、fork、工具过滤、输出 finalize |
| `src/services/compact/` | Auto-compact 阈值、compact prompt、micro-compact、post-compact 消息 |
| `src/utils/forkedAgent.ts` | createSubagentContext 隔离机制 |
| `src/memdir/` + `src/services/SessionMemory/` | 分层记忆、auto-memory 抽取、session memory |
| `src/services/api/claude.ts` | cache_control、buildSystemPromptBlocks、getCacheControl |
| `src/utils/claudemd.ts` + `src/context.ts` | CLAUDE.md 加载与注入为 user 消息 |

### 学术论文（备查）

| # | 论文 | 与 Pre-flight 的关联 |
|---|------|---------------------|
| 9 | CTA (arXiv:2603.21278) | 对话树架构解决 logical context poisoning——对 Pre-flight 的分支探索和回溯有参考价值 |
| 10 | CALM (arXiv:2502.08820) | 统一多轮对话与 tool use 的形式化框架 |
| 11 | ByteRover (arXiv:2604.01599) | 层级记忆架构——与 Claude Code 的 6 层记忆体系思路相似 |

---

## 附录 A: Claude Code 完整 Compaction Prompt

见 `src/services/compact/prompt.ts`，核心结构：

```
[NO_TOOLS_PREAMBLE]
CRITICAL: Respond with TEXT ONLY. Do NOT call any tools.

[BASE_COMPACT_PROMPT]
Your task is to create a detailed summary...
Sections: Primary Request / Key Technical Concepts / Files and Code /
          Errors and fixes / Problem Solving / All user messages /
          Pending Tasks / Current Work / Optional Next Step

[Optional: Additional Instructions from user/CLAUDE.md]

[NO_TOOLS_TRAILER]
REMINDER: Do NOT call any tools. Respond with plain text only.
```

## 附录 B: Claude Code 工具结果清理 (Micro-compact) 可压缩工具列表

```typescript
const COMPACTABLE_TOOLS = new Set([
  'Read',       // 文件读取结果
  'Bash',       // Shell 执行输出
  'Grep',       // 搜索结果
  'Glob',       // 文件匹配结果
  'WebSearch',  // 网页搜索结果
  'WebFetch',   // 网页抓取结果
  'Edit',       // 编辑确认
  'Write',      // 写入确认
])
```

清理后的占位文本: `[Old tool result content cleared]`

## 附录 C: 实施 Checklist

- [ ] **Phase 1 (P0)**: 定义 5 个 Pre-flight 工具的 JSON Schema
- [ ] **Phase 1 (P0)**: 实现 `PreflightState` 数据结构和 Belief State 跟踪
- [ ] **Phase 1 (P0)**: 构建 tool_use 解析器（含 qwen 兼容性测试）
- [ ] **Phase 2 (P1)**: 实现动态 system prompt 模板引擎
- [ ] **Phase 2 (P1)**: 实现收敛检测逻辑 (`convergenceScore` 计算)
- [ ] **Phase 2 (P1)**: 收敛指令注入 (`getConvergenceDirective`)
- [ ] **Phase 2 (P1)**: 接入 DashScope 显式缓存——在 system prompt 和每轮最后一条消息上标记 `cache_control: { type: "ephemeral" }`
- [ ] **Phase 3 (P2)**: 实现 micro-compact（选项详情清理）
- [ ] **Phase 3 (P2)**: 实现决策日志 (`DecisionLog`)
- [ ] **Phase 4 (P3)**: 实现 full compaction（结构化摘要）
- [ ] **Phase 4 (P3)**: 实现澄清模式切换工具和对应 prompt
- [ ] **Phase 5 (P4)**: 评估双模型策略的可行性和 ROI

---

> 报告完成。基于 Claude Code 源码仓库的深度分析和 Anthropic 官方资料的交叉验证，
> 本报告识别了 8 个可复用的设计模式，并为 Miragenty Pre-flight 提供了完整的优化方案。
> 核心改进按优先级排序为：Tool-as-Structure > Belief State > Dynamic Prompt > Convergence > **Prompt Caching** > Compaction。
>
> **更正 (2026-04-08)**: 经核实 DashScope 文档，qwen3.5-plus 支持与 Anthropic 兼容的
> `cache_control` 显式缓存（命中成本仅为标准价的 10%）。Prompt Caching 实施优先级
> 从 P4 提升至 **P1**，因为实施复杂度极低（仅需在请求中添加标记）且收益显著（~78% 成本降低）。
