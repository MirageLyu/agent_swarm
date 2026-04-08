# FM-12: Mission Report

> 版本: v1.0 | 日期: 2026-04-08  
> 优先级: P1 | 预估周期: 5-7 天  
> 依赖: FM-10（Contract 对比）、FM-11（Evaluator 评分数据） | 被依赖: FM-13  
> 原型参考: `design/prototypes/06-mission-report.html`

---

## IR — Initial Requirements

### IR-01: 用户故事

**US-01**: 作为开发者，我希望 Mission 完成后系统自动生成一份结构化的结案报告，这样我能快速了解整体执行情况。

**US-02**: 作为开发者，我希望报告包含每个任务的 Trade-off 记录（决策点和取舍原因），这样我能判断 Agent 的决策质量。

**US-03**: 作为开发者，我希望报告中可以看到 Contract 与实际执行的对比，这样我能评估 Agent 是否达成了预设目标。

**US-04**: 作为开发者，我希望能对报告中的决策进行投票（同意/不同意），这样我的反馈可以改进后续 Agent 行为。

**US-05**: 作为开发者，我希望报告可以导出为 Markdown/PDF，方便存档和分享。

### IR-02: 业务价值

- **信任建设**：报告是 Agent 向用户"述职"的核心载体
- **Trade-off 透明度**：差异化能力——展示 Agent 的实际决策过程
- **Learning Flywheel**：用户反馈数据驱动 Agent 行为迭代
- **Contract 闭环**：验收标准对照实际执行，形成完整质量链路

### IR-03: 高层验收标准

1. Mission 完成/失败后自动生成报告
2. 报告包含：执行摘要、架构决策、Evaluator 审查摘要、任务完成矩阵、已知限制、成本明细
3. 左侧 TOC 导航，右侧报告正文，支持节折叠
4. 架构决策支持用户投票（Agree/Disagree）
5. Contract 对照面板可打开/关闭
6. 报告可导出为 Markdown

---

## SR — Software Requirements

### 功能需求

#### FR-01: 报告生成引擎

- **FR-01.1**: 后端新增 `generate_mission_report` command，Mission 状态变为 `completed` 或 `failed` 时可触发
- **FR-01.2**: 报告生成逻辑汇聚以下数据源：
  - `missions` 表：标题、描述、总成本、时长
  - `tasks` 表：各任务状态、完成时间
  - `agents` 表：Agent 分配、tokens/cost
  - `evaluator_reviews` + `evaluator_annotations`：质量评分和注释
  - `mission_contracts`（如有）：验收标准
  - `cost_records`：成本明细
  - `agent_events`：执行历史（用于提取决策点）
- **FR-01.3**: 报告中的 Executive Summary 由 LLM 基于汇聚数据生成（1-2 段文字摘要）
- **FR-01.4**: Trade-off / Architecture Decisions 由 LLM 从 agent_events 中提取关键决策点
- **FR-01.5**: 报告数据序列化为 JSON 存入 `mission_reports` 表

#### FR-02: 报告视图

- **FR-02.1**: 新增 `ReportView` 视图，三栏布局：左 TOC（220px）、中 报告正文（max-width 780px）、右 Contract 对照面板（可选）
- **FR-02.2**: TOC 包含 7 个锚点：Executive Summary / Architecture Decisions / Evaluator Review / Task Matrix / Known Limitations / Cost Breakdown / Learning Flywheel
- **FR-02.3**: TOC 随正文滚动高亮当前节（scrollspy）
- **FR-02.4**: 每个节支持点击折叠/展开（max-height + opacity 动画）

#### FR-03: Executive Summary 节

- **FR-03.1**: 顶部卡片显示：任务名、状态 Badge（Completed/Failed）、6 组 metric（Duration、Cost $、Quality Score、Review Reduction Rate、Auto-fixes、Tasks completed）
- **FR-03.2**: 下方 1-2 段 LLM 生成的文字摘要

#### FR-04: Architecture Decisions 节

- **FR-04.1**: 每个决策卡片包含：ID（D-1, D-2...）、决策标题、Rationale、Trade-off、Risk
- **FR-04.2**: 每张卡片底部有 Agree / Disagree 投票按钮
- **FR-04.3**: 投票后显示计数（+1），按钮变为选中态
- **FR-04.4**: 投票结果存入 `report_votes` 表

#### FR-05: Evaluator Review Summary 节

- **FR-05.1**: 展示 Evaluator 审查轮次时间线：每轮显示 issues 数 / pass 状态
- **FR-05.2**: 每条审查发现带类别标签（Security/Bug/Performance 等）
- **FR-05.3**: Auto-fixed 项以绿色标记

#### FR-06: Task Completion Matrix 节

- **FR-06.1**: 表格列：Task、Agent、Score、Cost、Duration、Status
- **FR-06.2**: 按 Score 排序，低分高亮

#### FR-07: Cost Breakdown 节

- **FR-07.1**: 双列布局：By Model（Token 数 + 金额 + budget meter）和 By Task（横向 bar chart）
- **FR-07.2**: Budget meter 按百分比填充，>80% 变红

#### FR-08: Known Limitations 节

- **FR-08.1**: LLM 从执行过程提取的已知局限性列表
- **FR-08.2**: 每项含简短说明

#### FR-09: Learning Flywheel 节

- **FR-09.1**: 紫色左边框卡片，展示 Past Decision Patterns（从历史投票数据聚合）
- **FR-09.2**: 显示 insight 引用文字

#### FR-10: Contract 对照面板

- **FR-10.1**: Titlebar 提供 "Compare with Contract" 开关
- **FR-10.2**: 打开时右侧显示 overlay 面板，列出 Contract 各区块条目 + 达成状态
- **FR-10.3**: 未达成的条目红色标记

#### FR-11: 导出功能

- **FR-11.1**: Titlebar 提供 "Export Markdown" 按钮
- **FR-11.2**: 点击后将报告转为 Markdown 格式，通过 Tauri file dialog 保存到用户选择的路径
- **FR-11.3**: 导出成功显示 toast 提示

### 非功能需求

- **NFR-01**: 报告生成（含 LLM 摘要）≤ 30 秒
- **NFR-02**: 报告正文支持平滑滚动和 scrollspy，60fps
- **NFR-03**: 导出 Markdown 文件 ≤ 2 秒

### 接口需求

新增 Tauri Commands：

| Command | 参数 | 返回 | 说明 |
|---------|------|------|------|
| `generate_mission_report` | `{ mission_id }` | `{ report_id }` | 生成报告 |
| `get_mission_report` | `{ mission_id }` | `MissionReport` | 获取报告数据 |
| `vote_decision` | `{ report_id, decision_id, vote }` | `()` | 对决策投票 |
| `export_report_markdown` | `{ report_id, output_path }` | `()` | 导出 Markdown |

### 数据需求

新增 Schema 迁移：

```sql
CREATE TABLE IF NOT EXISTS mission_reports (
    id TEXT PRIMARY KEY,
    mission_id TEXT NOT NULL REFERENCES missions(id) ON DELETE CASCADE,
    report_data TEXT NOT NULL DEFAULT '{}',
    generated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS report_votes (
    id TEXT PRIMARY KEY,
    report_id TEXT NOT NULL REFERENCES mission_reports(id) ON DELETE CASCADE,
    decision_id TEXT NOT NULL,
    vote TEXT NOT NULL CHECK (vote IN ('agree', 'disagree')),
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(report_id, decision_id)
);
```

---

## AR — Architecture Requirements

### 前端组件设计

| 组件 | 路径 | 职责 |
|------|------|------|
| `ReportView` | `src/views/ReportView.tsx` | 报告视图容器 |
| `ReportTOC` | `src/components/report/ReportTOC.tsx` | 左侧目录 + scrollspy |
| `ExecSummarySection` | `src/components/report/ExecSummarySection.tsx` | 执行摘要节 |
| `DecisionCard` | `src/components/report/DecisionCard.tsx` | 架构决策卡片 + 投票 |
| `EvalTimelineSection` | `src/components/report/EvalTimelineSection.tsx` | Evaluator 时间线 |
| `TaskMatrixTable` | `src/components/report/TaskMatrixTable.tsx` | 任务完成矩阵表格 |
| `CostBreakdownSection` | `src/components/report/CostBreakdownSection.tsx` | 成本明细 + 图表 |
| `ContractCompareOverlay` | `src/components/report/ContractCompareOverlay.tsx` | Contract 对照面板 |

### 后端模块变更

| 文件 | 变更 |
|------|------|
| `commands/report.rs`（新） | 报告生成 + 查询 + 投票 + 导出 commands |
| `agent/report_generator.rs`（新） | 报告数据汇聚 + LLM 摘要生成逻辑 |
| `db/migrations.rs` | 新增 mission_reports、report_votes 表 |

### 与其他模块的交互

- **← FM-10**: Contract 内容用于对照面板
- **← FM-11**: Evaluator 评审数据填充质量相关节
- **← FM-04**: Cost 数据填充成本明细节
- **→ FM-13**: 投票数据纳入 Dashboard 的 Learning Flywheel 指标

### 现有代码关键入口

| 说明 | 文件路径 |
|------|---------|
| Mission 数据查询 | `src-tauri/src/commands/mission.rs` |
| Agent 事件查询 | `src-tauri/src/commands/agent.rs` |
| 成本数据 | `src-tauri/src/db/` cost_records 查询 |
| 前端 Sidebar 导航 | `src/components/Sidebar.tsx` |
| Tauri file dialog | `tauri-plugin-dialog`（已安装） |
