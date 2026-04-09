# FM-10 Pre-flight 多轮对话优化 — 子需求索引

> 版本: v1.1 | 日期: 2026-04-09  
> 调研基础: [Claude Code 架构分析报告](./claude-code-report.md) + [Agent-LLM 协同机制调研结论](#v11-新增内容)  
> 优化路线: [多轮对话优化路线](./multi-turn-optimization-roadmap.md)

---

## 概述

基于 Claude Code 架构逆向分析、Anthropic 官方文献和学术论文的调研成果，将 FM-10 Pre-flight 多轮对话优化拆分为 **6 个子需求模块**，按 P0 → P2 优先级排序，覆盖从基础结构改造到高级上下文管理的完整链路。

---

## 子需求全景

```
P0 ─── FM-10.1 Tool-as-Structure ──────────────────┐
       FM-10.2 Belief State & Convergence ──────────┤
                                                    │
P1 ─── FM-10.3 Dynamic System Prompt ──────────────┤
       FM-10.4 Prompt Caching ─────────────────────┤
                                                    │
P2 ─── FM-10.5 Context Compression ───────────────┤
       FM-10.6 Decision Log ───────────────────────┘
```

---

## 子需求列表

| ID | 名称 | 优先级 | 预估周期 | 核心目标 | 关键度量指标 |
|----|------|--------|---------|----------|-------------|
| [FM-10.1](./FM-10.1-tool-as-structure/) | Tool-as-Structure 工具即结构 | **P0** | 2-3 天 | 用 tool_use 替代 `---CHOICES---` 文本约定 | 选项解析成功率 70%→95%+ |
| [FM-10.2](./FM-10.2-belief-state-convergence/) | Belief State & Convergence 信念状态与收敛 | **P0** | 2 天 | 量化澄清进度，解决"不知何时停止" | 平均收敛轮次、过度提问率 40%→10% |
| [FM-10.3](./FM-10.3-dynamic-system-prompt/) | Dynamic System Prompt 动态提示拼装 | **P1** | 2 天 | LLM 每轮感知 Contract 状态和收敛进度 + Model Capability Registry | 重复提问率 20%→5% |
| [FM-10.4](./FM-10.4-prompt-caching/) | Prompt Caching 提示缓存 | **P1** | 1 天 | 利用 DashScope 显式缓存降低成本和延迟 | 8 轮对话成本降低 ≥55% |
| [FM-10.5](./FM-10.5-context-compression/) | Context Compression 上下文压缩 | **P2** | 2 天 | Micro-compact + Full compaction 支持长对话 | Token 节省 ≥25%，支持 ≥30 轮 |
| [FM-10.6](./FM-10.6-decision-log/) | Decision Log 决策日志 | **P2** | 1 天 | 记录决策理由和被否决方案，防重复建议 | 决策覆盖率 ≥90%，重复建议率 ≤3% |

**总预估周期: 10-12 天**

---

## 依赖关系

```
FM-10.1 Tool-as-Structure (P0)
  │
  ├──→ FM-10.2 Belief State (P0)
  │      │
  │      ├──→ FM-10.3 Dynamic Prompt (P1)
  │      │      │
  │      │      └──→ FM-10.4 Prompt Caching (P1)
  │      │
  │      └──→ FM-10.5 Context Compression (P2-P3)
  │
  └──→ FM-10.6 Decision Log (P2)
         │
         └──→ FM-10.3 (注入被否决方案段)
```

- **FM-10.1** 是基础：所有子需求都依赖 tool_use 格式
- **FM-10.2** 和 **FM-10.6** 可并行开发
- **FM-10.3** 依赖 FM-10.2 (注入 Belief State) 和 FM-10.6 (注入否决方案)
- **FM-10.4** 依赖 FM-10.3 的静态/动态分区
- **FM-10.5** 依赖 FM-10.1 的 tool_result 格式

---

## 推荐实施顺序

### Sprint 1 (第 1-3 天): 基础结构

1. **FM-10.1** Tool-as-Structure — 定义工具 Schema + 实现 tool_use 解析 + 保留 Fallback
2. **FM-10.2** Belief State — 数据结构 + 收敛计算 + 阶段转移

### Sprint 2 (第 4-6 天): 智能控制

3. **FM-10.3** Dynamic System Prompt — Prompt 分层 + 模板引擎 + Contract/BeliefState 注入
4. **FM-10.6** Decision Log — 决策记录 + 否决方案注入 + 前端时间线

### Sprint 3 (第 7-9 天): 性能优化

5. **FM-10.4** Prompt Caching — 缓存标记 + 效果监控
6. **FM-10.5** Context Compression — Micro-compact + Full compaction

### Sprint 4 (第 10 天): 集成验证

- 端到端测试所有子需求的协同工作
- A/B 对比测试优化前后的整体效果
- 修复集成问题

---

## 整体效果度量

### 优化前后对比实验设计

用 3 类需求场景各运行 3 次完整 Pre-flight，对比优化前（当前实现）和优化后（全部子需求完成）：

| 维度 | 优化前基线 | 优化后目标 | 度量方式 |
|------|-----------|-----------|----------|
| 选项解析成功率 | ~70% | ≥ 95% | 后端日志统计 |
| 平均收敛轮次（中等需求） | ~10 轮 | ≤ 8 轮 | 后端日志 |
| 进度条 100% 后仍提问的轮次 | 2-4 轮 | 0-1 轮 | 人工观察 |
| Agent 重复提问率 | ~20% | ≤ 5% | 人工标注 |
| Agent 重复建议已否决方案率 | ~15% | ≤ 3% | 人工标注 |
| 8 轮对话 token 成本 | 基线 | 降低 ≥ 55% | API usage 统计 |
| 首 token 延迟 (TTFT，第 2 轮起) | 基线 | 降低 ≥ 15% | 后端计时 |
| 支持的最大对话轮数 | ~15 轮 | ≥ 30 轮 | 压力测试 |
| Contract 条目有决策记录的比例 | 0% | ≥ 90% | DB 查询 |

### 用户体验综合评估

邀请 3-5 位测试者，用中等复杂度需求（"实现用户认证系统"）分别体验优化前后版本：

| 评估维度 | 评分方式 | 目标 |
|----------|----------|------|
| 对话流畅度 | 1-5 分 Likert 量表 | 优化后均分 ≥ 4.0 |
| 进度感知清晰度 | 1-5 分 | 优化后均分 ≥ 4.0 |
| Contract 质量信心 | 1-5 分 | 优化后均分 ≥ 3.5 |
| 签署决策信心 | 1-5 分 | 优化后均分 ≥ 4.0 |
| 整体满意度 | NPS (0-10) | ≥ 7 |

---

## 数据库迁移计划

| 迁移 ID | 对应子需求 | 变更 |
|---------|-----------|------|
| 010_belief_state | FM-10.2 | `preflight_sessions` 新增 `belief_state`, `convergence_score`, `phase` 列 |
| 011_decision_log | FM-10.6 | 新建 `decision_log` 表 |
| 012_compaction | FM-10.5 | `preflight_sessions` 新增 `compacted_at`, `compaction_summary`, `last_input_tokens`, `cumulative_input_tokens` 等列 |

---

## v1.1 新增内容

> 基于 Agent-LLM 协同机制调研结论 (2026-04-08) 的增量更新

### 新增到 FM-10.1

| 新增 FR | 内容 | 优先级 |
|---------|------|--------|
| FR-10.1.7 Response Prefilling | 预填充 assistant 消息开头引导 tool_use 格式响应，零成本提升遵从度 | P1 |
| FR-10.1.8 Parallel Tool Calls | 支持单轮多个 tool_calls（预留接口，暂不限制 add_contract_item 并行） | P3 |

### 新增到 FM-10.3

| 新增 FR | 内容 | 优先级 |
|---------|------|--------|
| FR-10.3.9 Model Capability Registry | 模型能力注册表，驱动 prompt 构建和响应解析的动态适配 | P1 |
| FR-10.3.10 Extended Thinking 适配 | Thinking API 与 CoT prompt 互斥规则 + 统一 `extract_reasoning()` 接口 | P2 |
| FR-10.3.11 核心设计原则 | "API 机制 > Prompt 技巧" + "能力检测驱动适配" + "静态稳定、动态灵活" | — |

### 新增到 FM-10.5

| 新增 FR | 内容 | 优先级 |
|---------|------|--------|
| FR-10.5.4a (更新) | Compaction 触发优先使用 API 返回的实际 token 数，而非字符估算 | P2 |
| FR-10.5.4.1 Token 预算追踪 | 每轮持久化 input/output token 用量，驱动 compaction 和预算控制 | P2 |

### 架构性影响

**Model Capability Registry** 是跨子需求的基础设施：

```
ModelCapabilities
  ├──→ FM-10.1: supports_tool_use → 决定用 tool_use 还是文本约定
  │    supports_prefill → 决定是否启用 Response Prefilling
  ├──→ FM-10.3: supports_thinking → 决定 prompt 中是否含 CoT 引导
  ├──→ FM-10.4: supports_prompt_caching → 决定是否注入 cache_control
  └──→ FM-10.5: (通过 token usage feedback) → 决定 compaction 触发
```

---

## 文档结构

```
docs/requirements/FM-10-preflight-contract/
├── requirements.md                          ← FM-10 主需求（已有）
├── test-cases.md                            ← FM-10 基础测试用例（已有）
├── claude-code-report.md                    ← 调研报告
├── multi-turn-optimization-roadmap.md       ← 优化路线图
├── research-prompt-claude-code.md           ← 调研 Prompt
├── sub-requirements-index.md                ← 本文件（子需求索引）
├── FM-10.1-tool-as-structure/
│   ├── requirements.md
│   └── test-cases.md
├── FM-10.2-belief-state-convergence/
│   ├── requirements.md
│   └── test-cases.md
├── FM-10.3-dynamic-system-prompt/
│   ├── requirements.md
│   └── test-cases.md
├── FM-10.4-prompt-caching/
│   ├── requirements.md
│   └── test-cases.md
├── FM-10.5-context-compression/
│   ├── requirements.md
│   └── test-cases.md
└── FM-10.6-decision-log/
    ├── requirements.md
    └── test-cases.md
```
