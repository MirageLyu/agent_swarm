# FM-13: Harness Dashboard

> 版本: v1.0 | 日期: 2026-04-08  
> 优先级: P2 | 预估周期: 5-7 天  
> 依赖: FM-11（质量评分数据）、FM-12（投票/反馈数据） | 被依赖: 无  
> 原型参考: `design/prototypes/07-harness-dashboard.html`

---

## IR — Initial Requirements

### IR-01: 用户故事

**US-01**: 作为开发者，我希望有一个全局仪表盘展示 Agent Swarm 的运营指标（成本、质量、效率、异常），这样我能持续优化 Harness 配置。

**US-02**: 作为开发者，我希望看到成本的实时走势和模型拆分，这样我能控制 LLM 开支。

**US-03**: 作为开发者，我希望系统能自动检测异常（成本飙升、Agent 超时、评分暴跌），这样我能及时介入。

**US-04**: 作为开发者，我希望看到 Learning Flywheel 指标（Review Reduction Rate、用户满意度趋势），这样我知道系统在不断改善。

### IR-02: 业务价值

- **可观测性**：Dashboard 是 Harness 工程的控制台，展示系统健康度
- **成本控制**：成本趋势和异常检测防止 LLM 开支失控
- **持续改善**：Learning Flywheel 指标量化系统进化速度
- **产品 Demo**：Dashboard 是展示 Harness 工程能力的关键页面

### IR-03: 高层验收标准

1. Dashboard 包含 4 个面板：Cost、Quality、Efficiency、Anomalies
2. Cost 面板有实时成本、预算进度条、Token 消耗折线图、模型成本明细
3. Quality 面板有平均分、Pass/Warn/Fail 分布、每任务评分、Evaluator 活动日志
4. Efficiency 面板有 Agent 利用率、并行时间线（甘特图）、加速倍数
5. Anomalies 面板自动标记异常项
6. 支持 Live / History 模式和时间范围选择
7. 面板可折叠，折叠后显示紧凑摘要

---

## SR — Software Requirements

### 功能需求

#### FR-01: Dashboard 布局

- **FR-01.1**: 新增 InsightsView（或替换现有占位），2×2 网格布局，4 个可折叠面板
- **FR-01.2**: 每个面板有 header（图标 + 标题 + 核心指标）和 body（详细图表）
- **FR-01.3**: 面板折叠时 header 保留、body 隐藏，显示 compact-metrics 一行摘要
- **FR-01.4**: 面板之间 1px 分隔线

#### FR-02: TopBar 控件

- **FR-02.1**: Topbar 显示 Live / History 分段切换
- **FR-02.2**: Live 模式：3 秒轮询刷新数据 + 绿色 Live 脉冲点
- **FR-02.3**: History 模式：停止轮询，显示静态快照
- **FR-02.4**: 时间范围按钮组：Last 10m / 30m / 1h / All

#### FR-03: Cost 面板

- **FR-03.1**: 顶部大数字显示当前 Mission/全局累计成本
- **FR-03.2**: Budget Usage 进度条：百分比填充 + 颜色阈值（>50% 橙、>80% 红）
- **FR-03.3**: Token Consumption 折线图（SVG 绘制），x 轴时间、y 轴 token 数
- **FR-03.4**: Model Breakdown 表格：Model 名、Tokens、Cost
- **FR-03.5**: Per-Agent Cost 横向条形图

#### FR-04: Quality 面板

- **FR-04.1**: 顶部显示平均质量评分（大数字）
- **FR-04.2**: Stacked bar chart：Pass / Warn / Fail 分布
- **FR-04.3**: 每任务评分垂直条形图（T1-TN）
- **FR-04.4**: Evaluator Activity 日志列表（最近 5 条评审事件）
- **FR-04.5**: Auto-Fix Success Rate Badge

#### FR-05: Efficiency 面板

- **FR-05.1**: Agent Utilization 百分比（大数字）+ Active Agents 数
- **FR-05.2**: 每个 Agent 的圆形进度条（环形 SVG，stroke-dashoffset 动画）
- **FR-05.3**: 甘特图时间线：展示各 Agent/Task 的并行执行时段
- **FR-05.4**: Speedup Callout：显示 "串行时间 → 实际时间，X× 加速"

#### FR-06: Anomalies 面板

- **FR-06.1**: 顶部显示当前异常数（0 则绿色"All Clear"、>0 则橙色计数）
- **FR-06.2**: 每个异常卡片：标题 + 严重度 Badge（如"3.2× median"）+ 详情描述
- **FR-06.3**: 异常类型分类标签：`cost_spike`、`timeout`、`quality_drop`、`error_rate`
- **FR-06.4**: 异常时间线：节点展示异常发生过程

#### FR-07: 异常检测逻辑

- **FR-07.1**: 后端新增 `detect_anomalies` command
- **FR-07.2**: 检测规则（Phase 2 MVP 使用静态阈值）：
  - 单步成本 > 中位数 × 3 → `cost_spike`
  - Agent 执行时间 > 10 分钟 → `timeout`
  - Evaluator 评分 < 5 → `quality_drop`
  - 连续 3 次 tool_result 失败 → `error_rate`
- **FR-07.3**: 异常数据存入 `anomaly_records` 表

### 非功能需求

- **NFR-01**: Live 模式下轮询刷新不导致 UI 闪烁
- **NFR-02**: SVG 图表在 100 个数据点内流畅渲染
- **NFR-03**: 面板折叠/展开动画 ≤ 300ms
- **NFR-04**: 异常检测查询 ≤ 500ms

### 接口需求

新增 Tauri Commands：

| Command | 参数 | 返回 | 说明 |
|---------|------|------|------|
| `get_dashboard_data` | `{ time_range, mission_id? }` | `DashboardData` | 获取全部面板数据 |
| `detect_anomalies` | `{ mission_id? }` | `Anomaly[]` | 运行异常检测 |
| `get_efficiency_metrics` | `{ mission_id }` | `EfficiencyMetrics` | Agent 利用率 + 甘特数据 |

### 数据需求

新增 Schema 迁移：

```sql
CREATE TABLE IF NOT EXISTS anomaly_records (
    id TEXT PRIMARY KEY,
    mission_id TEXT REFERENCES missions(id) ON DELETE CASCADE,
    agent_id TEXT REFERENCES agents(id) ON DELETE CASCADE,
    type TEXT NOT NULL
        CHECK (type IN ('cost_spike', 'timeout', 'quality_drop', 'error_rate')),
    severity TEXT NOT NULL DEFAULT 'warning'
        CHECK (severity IN ('info', 'warning', 'critical')),
    title TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    metric_value REAL,
    threshold_value REAL,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
```

聚合查询（不需要新表，基于现有表 JOIN）：
- 成本趋势：`cost_records GROUP BY date`
- 质量分布：`evaluator_reviews GROUP BY score_range`
- Agent 利用率：`agents` running 时间 / 总时间
- 甘特图：`agents.created_at` 到 `agents.updated_at` 时间段

---

## AR — Architecture Requirements

### 前端组件设计

| 组件 | 路径 | 职责 |
|------|------|------|
| `InsightsView` | `src/views/InsightsView.tsx`（替换占位） | Dashboard 容器 + 2×2 网格 |
| `DashboardTopBar` | `src/components/dashboard/DashboardTopBar.tsx` | Live/History 切换 + 时间范围 |
| `CostPanel` | `src/components/dashboard/CostPanel.tsx` | 成本面板 |
| `QualityPanel` | `src/components/dashboard/QualityPanel.tsx` | 质量面板 |
| `EfficiencyPanel` | `src/components/dashboard/EfficiencyPanel.tsx` | 效率面板 |
| `AnomalyPanel` | `src/components/dashboard/AnomalyPanel.tsx` | 异常面板 |
| `TokenChart` | `src/components/dashboard/TokenChart.tsx` | SVG 折线图 |
| `BarChart` | `src/components/dashboard/BarChart.tsx` | SVG 条形图 |
| `GanttTimeline` | `src/components/dashboard/GanttTimeline.tsx` | 甘特时间线 |
| `CircleProgress` | `src/components/dashboard/CircleProgress.tsx` | 环形进度条 |
| `AnomalyCard` | `src/components/dashboard/AnomalyCard.tsx` | 异常卡片 |

### 后端模块变更

| 文件 | 变更 |
|------|------|
| `commands/dashboard.rs`（新） | Dashboard 数据聚合 + 异常检测 commands |
| `db/migrations.rs` | 新增 anomaly_records 表 |

### 与其他模块的交互

- **← FM-04**: 复用 cost_records 表的成本数据
- **← FM-11**: 复用 evaluator_reviews 的质量评分数据
- **← FM-12**: 复用 report_votes 的用户反馈数据
- **← FM-02**: Agent 状态数据用于效率计算

### 现有代码关键入口

| 说明 | 文件路径 |
|------|---------|
| InsightsView 占位组件 | `src/views/InsightsView.tsx` |
| 成本数据表 | `src-tauri/src/db/migrations.rs` (cost_records) |
| Agent 数据表 | `src-tauri/src/db/migrations.rs` (agents) |
| Evaluator 数据表 | FM-11 新增的 evaluator_reviews |
| Sidebar 导航 | `src/components/Sidebar.tsx`（Insights 项已存在） |
