# Phase 2 — 原型设计待实现清单

> 日期: 2026-04-07  
> 来源: `design/prototypes/` 与当前实现的对比分析  
> 前置: Phase 1 FM-01 ~ FM-09 全部完成后启动

---

## 概述

以下原型设计中的功能**不属于 Phase 1 范围**，需在 Phase 2 中规划需求并拆分为新的 FM。它们涉及 Pre-flight 对话式规划、Evaluator 智能审查、Mission 结案报告和 Harness 仪表盘等高级能力。

---

## 1. Pre-flight 对话 & Mission Contract（原型 02）

**原型文件**: `design/prototypes/02-preflight-chat.html`

**功能描述**:
- 用户与 Planner Agent 的多轮对话式需求澄清界面
- 三阶段流程：Scenario Walk（情景推演）→ Devil's Advocate（异议挑战）→ Risk Highlighter（风险标注）
- Mission Contract 构建器：将澄清结果结构化为正式的"任务合同"
- Contract 包含：Scope（范围）、Constraints（约束）、Acceptance Criteria（验收标准）、Risk Items（风险项）
- Contract 确认后才进入 Task DAG 生成阶段

**涉及后端能力**:
- Planner Agent 的多轮对话模式（当前仅单轮）
- Contract 数据模型和持久化
- Contract → Task DAG 的转换逻辑

**建议 FM 编号**: FM-10 Pre-flight & Mission Contract

---

## 2. Evaluator 注释与质量评分（原型 05 部分）

**原型文件**: `design/prototypes/05-code-review.html`（未在 Phase 1 中实现的部分）

**功能描述**:
- Evaluator Agent 对每个 Agent 产出自动审查，生成行级注释（类似 GitHub PR review comments）
- 每个文件/Agent 的质量评分（0-100）
- 评分维度：代码正确性、风格一致性、测试覆盖、安全性
- 注释分类标记：`bug`、`style`、`performance`、`security`、`suggestion`
- 基于注释的自动修复建议（Evaluator → Agent 反馈循环）

**涉及后端能力**:
- Evaluator Agent 引擎（独立的 Agent 类型，审查而非编码）
- 注释数据模型（关联到文件行）
- 评分算法与权重配置

**建议 FM 编号**: FM-11 Evaluator Agent & Quality Scoring

---

## 3. Mission Report 结案报告（原型 06）

**原型文件**: `design/prototypes/06-mission-report.html`

**功能描述**:
- Mission 完成后自动生成结构化结案报告
- 报告包含：
  - Executive Summary（执行摘要）
  - Task 执行时间线（甘特图形式）
  - 每个 Task 的 Trade-off 记录（决策点 + 取舍原因）
  - 变更范围摘要（文件列表 + diff 统计）
  - 成本明细（按 Agent/Task 维度）
  - 未完成项 & 风险残留
  - 用户反馈入口（thumbs up/down + 文字评价）
- 报告可导出为 Markdown/PDF
- 用户反馈数据回流到 Learning Flywheel（改善后续 Agent 行为）

**涉及后端能力**:
- 报告生成引擎（汇聚多表数据）
- Trade-off 记录机制（Agent 执行过程中主动记录决策点）
- 用户反馈收集与存储
- Markdown/PDF 导出

**建议 FM 编号**: FM-12 Mission Report

---

## 4. Harness Dashboard 仪表盘（原型 07）

**原型文件**: `design/prototypes/07-harness-dashboard.html`

**功能描述**:
- 全局 Harness 运营仪表盘，包含：
  - Agent 健康度面板：每个 Agent 类型的成功率、平均耗时、成本趋势
  - Tool 使用统计：每个工具的调用频次、成功率、平均耗时
  - 成本趋势图：日/周/月维度的成本走势
  - 异常检测面板：自动标记异常高的成本、超时、失败率
  - Learning Flywheel 指标：Review Reduction Rate 趋势、用户反馈满意度
- 时间范围选择器（最近 7 天 / 30 天 / 全部）
- 数据钻取：点击指标 → 跳转到对应 Mission/Agent 详情

**涉及后端能力**:
- 聚合查询引擎（按时间、Agent、Tool 维度）
- 时序数据存储（或从现有表聚合）
- 异常检测算法（统计阈值或简单 Z-score）

**建议 FM 编号**: FM-13 Harness Dashboard

---

## 5. Approval Queue 审批队列（原型 01 部分）

**原型文件**: `design/prototypes/01-commander-shell.html`（未在 Phase 1 中实现的部分）

**功能描述**:
- 主界面中间面板展示"待审批"队列（Agent 请求人工决策的事项）
- 队列项类型：破坏性操作授权、超出 scope 的任务确认、成本超预算确认
- 每项包含：Agent 名、请求原因、上下文摘要、操作按钮（Approve / Reject / Defer）
- 计数 Badge 在 TopBar 和 Sidebar 同步显示

**涉及后端能力**:
- 审批请求数据模型
- Agent 执行中的暂停等待人工审批机制
- 审批结果回传给 Agent 继续执行

**建议 FM 编号**: FM-14 Approval Queue

---

## Phase 2 建议开发顺序

```
Sprint 6:  FM-10 Pre-flight & Mission Contract
           （提升任务规划质量，为 Evaluator 提供验收基准）

Sprint 7:  FM-11 Evaluator Agent & Quality Scoring
           （核心差异化能力，自动代码审查）

Sprint 8:  FM-12 Mission Report + FM-14 Approval Queue
           （信任建设闭环 + 权限控制）

Sprint 9:  FM-13 Harness Dashboard
           （运营洞察，需要积累足够历史数据）
```

---

## 注意事项

- 以上功能需求仅为原型分析得出的初步范围，**正式开发前需按 FM 规范编写完整的 IR/SR/AR 和 test-cases**
- Phase 2 的每个 FM 可能依赖 FM-09 建立的 UI 骨架（如 Command Palette 注册新命令、TopBar 增加 Approval 计数等）
- Evaluator Agent（FM-11）是产品差异化的关键，建议优先投入设计
