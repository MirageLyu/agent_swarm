# FM-10 多轮对话优化路线

> 创建日期: 2026-04-08
> 状态: 规划中
> 参考来源: Anthropic Context Engineering (2025.09), CALM (arXiv:2502.08820), CTA (arXiv:2603.21278), Mem0 State of AI Agent Memory 2026, JetBrains Research 2025.12

---

## 优化方向全景

```
L4 架构层   ── 7. Sub-agent / CTA（子Agent与对话树）
L3 记忆层   ── 6. Memory Architecture（层级记忆架构）
              5. Context Compression（上下文压缩）
L2 控制层   ── 4. Conversation Flow Control（对话流程编排）
              3. Dialogue State Tracking / DST（对话状态追踪）
L1 基础层   ── 2. Structured Output（结构化输出 / tool_use）
              1. Context Engineering（上下文工程）
```

---

## 1. Context Engineering（上下文工程）

**来源**: Anthropic "Effective context engineering for AI agents" (2025.09)

**核心**: 从"写好 prompt"进化为"管理进入 context window 的每一个 token 的信号密度"。LLM 存在 context rot——随 token 数增加，注意力准确性下降。context 是有限资源，边际收益递减。

**当前问题**: system prompt 固定文本；全量历史消息平铺传入；无信号密度筛选。

**改进方向**:
- 每轮动态拼装 system prompt：角色定义 + Contract 当前状态 + 轮次信息 + 收敛指令
- 按信号密度筛选历史：近 N 轮完整保留，远期轮次只保留决策结论
- 工具结果精简化

**优先级**: P1（效果立竿见影，改动量中等）

---

## 2. Structured Output（结构化输出）

**来源**: CALM 论文 (arXiv:2502.08820); OpenAI/Anthropic structured output API

**核心**: 不依赖文本约定（`---CHOICES---`），利用 LLM 原生 function calling / tool_use 返回结构化数据。

**改进方向**:
- 定义 `present_choices` 工具：LLM 通过 tool_use 返回选项
- 定义 `add_contract_item` 工具：LLM 直接调用添加 Contract 条目
- 定义 `suggest_sign` 工具：LLM 判断澄清充分时调用建议签署
- 彻底解决 choices 解析脆弱问题

**优先级**: P1

---

## 3. Dialogue State Tracking / DST（对话状态追踪）

**来源**: Stanford SLP3; ACM Survey 2025 (doi:10.1145/3771090); EMNLP 2025

**核心**: 维护结构化 belief state 记录已确认信息、待确认 slot、未触及领域。每轮更新而非只看消息文本。

**Miragenty BeliefState 设计**:
```json
{
  "scope": ["已确认条目..."],
  "constraints": ["已确认条目..."],
  "exclusions": ["已确认条目..."],
  "assumptions": ["已确认条目..."],
  "pending_topics": ["认证方式", "数据存储"],
  "current_topic": "认证方式",
  "round": 5,
  "clarification_quality": 0.7
}
```

每轮调用前注入 system prompt，LLM 获得"位置感"和"收敛依据"。

**优先级**: P1（与 Context Engineering 一起实现）

---

## 4. Conversation Flow Control（对话流程编排）

**来源**: GUS frame-based dialogue (Stanford); Elicitation pattern

**核心**: 程序定义对话阶段和转移条件，LLM 在阶段内自由提问，阶段跳转由代码控制。

**阶段模型**:
```
阶段1: 功能范围   →（scope>=2）→ 阶段2: 技术约束
阶段2: 技术约束   →（constraints>=1）→ 阶段3: 边界排除
阶段3: 边界排除   →（exclusions>=1）→ 阶段4: 风险假设
阶段4: 风险假设   →（assumptions>=1）→ 阶段5: 确认总结
阶段5: 确认总结   → 签署 Contract
```

**优先级**: P2（需要一些设计但代码量不大）

---

## 5. Context Compression（上下文压缩）

**来源**: Anthropic "Compaction"; JetBrains Research 2025.12; Mem0

**三个层次**:

| 技术 | 做法 | 压缩率 | 适用场景 |
|------|------|--------|---------|
| Tool result clearing | 删除历史中工具调用原始结果，只保留结论 | ~30% | 已完成的工具调用 |
| Sliding window + Summary | 近 N 轮完整保留，远期用 LLM 摘要压缩 | ~60% | 超过 8 轮的长对话 |
| Compaction | 整个对话压缩为结构化摘要，重开 context window | ~90% | 超过 20 轮或接近 context limit |

**优先级**: P3（长期架构投资）

---

## 6. Hierarchical Memory Architecture（层级记忆架构）

**来源**: Mem0 State of AI Agent Memory 2026; ByteRover (arXiv:2604.01599)

**三层记忆**:
```
Working Memory   ← context window 内（当前 3-5 轮 + belief state）
Short-term       ← 本次 session 的 SQLite 记录（完整历史）
Long-term        ← 跨 session 的知识（用户偏好、项目上下文）
```

ByteRover 用 LLM 管理 Context Tree（Domain >> Topic >> Subtopic >> Entry），带重要性评分和衰减机制。

**优先级**: P4（Phase 3 架构投资）

---

## 7. Sub-agent Architecture & Conversation Tree

**来源**: Anthropic "Sub-agent architectures"; CTA 论文 (arXiv:2603.21278, 2026.03)

**Sub-agent**: 主 Agent 编排，子 Agent 深度探索后只返回精简摘要。三种模式可拆为独立子 Agent。

**CTA**: 对话组织为树形结构，每个节点独立 context window，避免 logical context poisoning。模式切换用分支节点实现。

**优先级**: P5（Phase 3+）

---

## 实施路线

```
P0: DTS 修复（当前）
 ↓
P1: Context Engineering + DST + Structured Output
    - 动态 system prompt（注入 BeliefState + 轮次 + Contract 状态）
    - tool_use 替代 ---CHOICES--- 文本约定
    - 预计工作量: 2-3 天
 ↓
P2: Conversation Flow Control
    - 阶段化对话 + 收敛条件
    - 预计工作量: 1-2 天
 ↓
P3: Context Compression
    - sliding window + 摘要
    - 预计工作量: 1 天
 ↓
P4-P5: Memory Architecture + Sub-agent / CTA
    - 长期架构投资
    - 预计工作量: 3-5 天
```

---

## 参考文献

1. Anthropic. "Effective context engineering for AI agents." 2025.09
2. CALM: Conversational Agentic Language Model. arXiv:2502.08820
3. CTA: Conversation Tree Architecture. arXiv:2603.21278, 2026.03
4. Mem0. "State of AI Agent Memory 2026"
5. ByteRover: Agent-Native Memory Through LLM-Curated Hierarchical Context. arXiv:2604.01599
6. JetBrains Research. "Cutting Through the Noise: Smarter Context Management for LLM." 2025.12
7. ACM. "A Survey on Recent Advances in LLM-Based Multi-turn Dialogue Systems." doi:10.1145/3771090
8. University of Mannheim. "Context Engineering." IE685 FSS2026 Lecture 06
