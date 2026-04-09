# 调研 Prompt: Claude Code 架构分析 → Miragenty Pre-flight 优化

> 用途: 提供给调研 subagent 使用
> 创建日期: 2026-04-08

---

## 你的角色

你是一名 AI Agent 架构研究员。你的任务是深入分析 Claude Code（Anthropic 官方 CLI Agent）的多轮对话架构，提取可用于优化 Miragenty Pre-flight 模式的关键设计模式和技术方案。

## 背景：Miragenty Pre-flight 模式

Miragenty 是一个 AI Agent Swarm 指挥客户端（Tauri 2.0 + React 19）。其中 **Pre-flight 模式** 是一个多轮对话式需求澄清功能：

- 用户输入高层需求（如"实现用户认证系统"）
- Planner Agent 通过多轮对话逐步澄清需求（技术选型、边界排除、风险假设等）
- 对话过程实时构建 Mission Contract（Scope / Constraints / Exclusions / Assumptions 四区块）
- 签署 Contract 后基于其内容生成高质量 Task DAG

**当前架构的问题**:
1. 全量历史回放：每轮把所有消息原文传入 LLM，无压缩、无筛选
2. 固定 system prompt：不注入 Contract 状态、轮次信息、收敛指令
3. 文本约定结构化输出：用 `---CHOICES---` 分隔符 + JSON 约定，LLM 不遵守时解析失败
4. 无对话状态追踪：没有 belief state，LLM 不知道哪些 slot 已确认、哪些待填
5. 无上下文压缩：长对话 token 成本高，context rot 导致质量下降
6. 无收敛机制：LLM 不知道何时停止提问

## 调研目标

针对以下 7 个维度，分析 Claude Code 的实现方式，并输出 Miragenty 可直接采用的设计方案。

---

### 维度 1: Agentic Loop 与对话状态管理

**调研问题**:
- Claude Code 的 agentic loop 如何决定何时停止（`stop_reason = end_turn` vs `tool_use`）？
- 是否有显式的对话状态机或 belief state？还是完全靠 LLM 自主判断？
- 如何确定"任务完成"？有没有程序化的退出条件？

**映射到 Pre-flight**: 我们需要让 Agent 知道"澄清已充分，可以建议签署"。是靠 prompt 还是靠程序判断？

### 维度 2: Context Engineering — System Prompt 动态构建

**调研问题**:
- Claude Code 的 system prompt 是完全静态的，还是每轮动态拼装？
- `CLAUDE.md` 内容是如何注入的（作为 system prompt 的一部分？作为 user message 的 `system-reminder`？）
- system prompt 中是否包含当前状态信息（已完成的步骤、当前目标等）？
- 有没有 `cache_control: ephemeral` 的使用模式？哪些内容被标记为 ephemeral？

**映射到 Pre-flight**: 我们需要在 system prompt 中注入 Contract 当前状态（已有哪些条目）、轮次信息、收敛指令。

### 维度 3: Tool Use 替代文本约定

**调研问题**:
- Claude Code 如何通过 tool_use 实现结构化输出？
- 工具定义的 JSON schema 是什么设计原则？（原文提到"minimal overlap in functionality"）
- tool_use 的 `id` 和 `tool_result` 的配对机制如何工作？
- 工具数量控制在什么范围？过多工具会怎样？

**映射到 Pre-flight**: 我们计划用 tool_use 替代 `---CHOICES---` 文本约定。需要设计 `present_choices`、`add_contract_item`、`suggest_sign` 等工具的 schema。

### 维度 4: Sub-agent 架构与上下文隔离

**调研问题**:
- Claude Code 何时决定 spawn sub-agent vs 自己处理？（从逆向工程文章看到 "not a needle query" 才用 sub-agent）
- Sub-agent 的 system prompt 和 tool list 与主 agent 有何不同？
- Sub-agent 的输出如何传回主 agent？（从逆向看到是 `tool_result` 返回 markdown + agentId）
- Sub-agent 使用不同（更便宜）的模型是刻意的设计选择吗？
- 上下文隔离的核心收益是什么？Anthropic 的数据说"80% of success came from token volume advantage"

**映射到 Pre-flight**: 三种澄清模式（场景走查 / 魔鬼代言人 / 风险标记）是否应该拆为独立 sub-agent？

### 维度 5: Compaction 与上下文压缩

**调研问题**:
- Claude Code 的 compaction 触发时机是什么？（接近 context window 上限时？固定间隔？）
- Compaction 的 prompt 是什么？它保留什么、丢弃什么？
- "Tool result clearing" 具体怎么做？是完全删除还是保留调用记录？
- Compaction 后重新开始的 context window 包含什么？（summary + 最近 N 条 + 最近 5 个文件？）
- 有没有 compaction 失败（丢失关键信息）的 fallback 机制？

**映射到 Pre-flight**: 超过 8 轮的 pre-flight 对话需要压缩。需要确定压缩策略和触发条件。

### 维度 6: 结构化笔记（Structured Note-taking / Memory）

**调研问题**:
- Claude Code 的 `CLAUDE.md` 和 Memory tool 分别解决什么问题？
- Memory tool 存储的是什么格式？存在哪里？（文件系统 markdown？数据库？）
- Agent 在什么时候写笔记？是主动的还是指令驱动的？
- 笔记如何被后续轮次检索和注入？是全量注入还是按需检索？
- Claude playing Pokémon 的 memory 案例中，"precise tallies across thousands of steps" 是怎么实现的？

**映射到 Pre-flight**: Contract 本身就是一种结构化笔记。但我们可能还需要额外的笔记机制来记录"为什么做出某个决策"。

### 维度 7: Prompt Caching 与性能优化

**调研问题**:
- Claude Code 为什么用 `max_tokens: 1` 的 dummy request 做 cache warmup？
- `cache_control: ephemeral` 标记的具体语义是什么？哪些内容应该被缓存？
- System prompt + tool definitions 作为稳定前缀被缓存的模式是怎样的？
- 轻量 LLM vs 重量 LLM 的分工策略是什么？metadata 用轻量、主任务用重量、sub-agent 用轻量？

**映射到 Pre-flight**: 我们当前使用 DashScope qwen3.5-plus 单模型。是否应该引入双模型策略？

---

## 调研资源

以下是你应该重点分析的公开资料：

### 一手来源（Anthropic 官方）
1. **Anthropic Blog**: "Effective context engineering for AI agents" (2025.09) — https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents
2. **Anthropic Blog**: "Building effective AI agents" — https://www.anthropic.com/research/building-effective-agents
3. **Anthropic Blog**: "How we built our multi-agent research system" — https://www.anthropic.com/engineering/multi-agent-research-system
4. **Anthropic Blog**: "Writing tools for AI agents – with AI agents" — https://www.anthropic.com/engineering/writing-tools-for-agents
5. **Claude Platform Docs**: Context engineering cookbook — https://platform.claude.com/cookbook/tool-use-context-engineering-context-engineering-tools
6. **Claude Code Best Practices**: https://code.claude.com/docs/en/best-practices

### 逆向工程分析
7. **George Sung**: "Tracing Claude Code's LLM Traffic: Agentic loop, sub-agents, tool use, prompts" (2026.01) — https://medium.com/@georgesung/tracing-claude-codes-llm-traffic-agentic-loop-sub-agents-tool-use-prompts-7796941806f5
   - 包含完整的 main agent system prompt 和 tool list
   - 包含 Explore sub-agent 的 system prompt 和 tool list
   - 包含实际 LLM request/response 流量记录
   - 附录有完整的 gist 链接
8. **Vikash Rungta**: "Claude Code Architecture (Reverse Engineered)" — https://vrungta.substack.com/p/claude-code-architecture-reverse
9. **Penligent**: "Inside Claude Code: Architecture Behind Tools, Memory, Hooks and MCP" — https://www.penligent.ai/hackinglabs/es/inside-claude-code-the-architecture-behind-tools-memory-hooks-and-mcp/
10. **Kotrotsos**: "Claude Code Internals, Part 13: Context Management" — https://kotrotsos.medium.com/claude-code-internals-part-13-context-management-ffa3f4a0f6b4

### 学术论文
11. **CTA**: Conversation Tree Architecture (arXiv:2603.21278, 2026.03) — 对话树架构，解决 logical context poisoning
12. **CALM**: Conversational Agentic Language Model (arXiv:2502.08820) — 统一多轮对话与 tool use
13. **ByteRover**: Agent-Native Memory Through LLM-Curated Hierarchical Context (arXiv:2604.01599) — 层级记忆架构

---

## 输出要求

请输出一份结构化的调研报告，包含以下部分：

### 1. Claude Code 架构解析
对每个维度，提供 Claude Code 的具体实现方式（附代码片段或配置示例）。

### 2. 关键设计模式提取
提取 5-8 个可直接复用的设计模式，每个模式包含：
- 模式名称
- 在 Claude Code 中的实现
- 对 Miragenty Pre-flight 的适用性评估（高/中/低）
- 推荐的实现方案（含伪代码或架构图）

### 3. Pre-flight 优化方案
基于调研结果，为 Miragenty Pre-flight 模式设计一套完整的优化方案：
- 改造后的 system prompt 模板（动态部分用 `{{placeholder}}` 标注）
- Tool schema 定义（JSON）
- Compaction 策略
- 对话状态追踪数据结构
- 实施优先级排序

### 4. 风险与取舍
列出每个优化方向的潜在风险和取舍（如 tool_use 增加 token 消耗、compaction 可能丢失信息等）。

### 5. 参考文献索引
所有引用的资料来源及其关键结论。
