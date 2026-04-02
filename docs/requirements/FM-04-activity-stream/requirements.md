# FM-04: Activity Stream & Cost Tracking

> 版本: v1.0 | 日期: 2026-04-01  
> 优先级: P1 | 预估周期: 3-4 天  
> 依赖: FM-02, FM-03 | 被依赖: 无

---

## IR — Initial Requirements

### IR-01: 用户故事

**US-01**: 作为开发者，我希望能同时看到多个 Agent 的实时活动，这样我知道 swarm 正在做什么。

**US-02**: 作为开发者，我希望能看到每个 Agent 和每个任务花了多少 token / 成本，这样我能判断是否失控。

**US-03**: 作为开发者，我希望在应用重启后仍能查看活动历史和成本数据。

### IR-02: 业务价值

- 活动流是产品的“战场态势图”
- 成本可视化是 harness 的核心控制能力之一
- 为后续预算系统和异常检测打基础

### IR-03: 高层验收标准

1. 同时展示多个 Agent 的事件与流式输出
2. 每个 Agent 显示 tokens、cost、step、状态
3. 支持切换实时视图和历史回放视图
4. Mission 级汇总成本可见

---

## SR — Software Requirements

### 功能需求

#### FR-01: 多 Agent 活动流

- **FR-01.1**: Workspace 支持列表视图和聚焦视图两种模式
- **FR-01.2**: 列表视图下每个 Agent 卡片展示最近事件、当前状态、当前 step
- **FR-01.3**: 聚焦视图下展示完整事件时间线和文本流
- **FR-01.4**: 支持按 Mission 过滤 Agent
- **FR-01.5**: 支持从数据库加载历史事件，而不只依赖实时订阅

#### FR-02: 成本追踪展示

- **FR-02.1**: 每次 checkpoint 显示本步 input/output tokens
- **FR-02.2**: 每个 Agent 卡片显示累计 tokens 和累计 cost
- **FR-02.3**: Workspace 顶部显示当前 Mission 总 cost
- **FR-02.4**: 支持按 Agent 维度查看成本明细

#### FR-03: 状态与异常可见化

- **FR-03.1**: 用颜色区分 `running/completed/failed/cancelled/waiting`
- **FR-03.2**: tool error、schema error、cancelled 事件在 UI 中明确突出
- **FR-03.3**: 若成本超过预设阈值，显示 warning 样式（Phase 1 仅静态阈值）

### 非功能需求

- **NFR-01**: 10 个 Agent 同时推流时页面保持可交互
- **NFR-02**: 单个 Agent 历史事件 500 条内滚动流畅
- **NFR-03**: 数值展示口径统一，cost 保留 4 位小数

### 接口需求

新增 Commands：

| Command | 参数 | 返回 | 说明 |
|---------|------|------|------|
| `get_mission_cost_summary` | `{ mission_id }` | `{ total_cost, total_input_tokens, total_output_tokens }` | Mission 汇总成本 |
| `list_agent_events` | `{ mission_id?, agent_id? }` | `AgentEvent[]` | 查询活动流历史 |

### 数据需求

- `cost_records` 成为真实成本来源
- `agent_events` 与 `agents.tokens_used/cost_usd` 保持一致

---

## AR — Architecture Requirements

### 前端组件设计

| 组件 | 路径 | 职责 |
|------|------|------|
| `WorkspaceView` | `src/views/WorkspaceView.tsx` | 容器，负责筛选/视图切换 |
| `AgentStreamList` | `src/components/workspace/AgentStreamList.tsx` | 多 Agent 列表 |
| `AgentStreamCard` | `src/components/workspace/AgentStreamCard.tsx` | 单 Agent 摘要卡片 |
| `AgentTimeline` | `src/components/workspace/AgentTimeline.tsx` | 时间线展示 |
| `CostSummaryBar` | `src/components/workspace/CostSummaryBar.tsx` | Mission 汇总成本 |

### 后端模块变更

| 文件 | 变更 |
|------|------|
| `commands/agent.rs` | 新增查询事件与成本汇总命令 |
| `db/*` | 增加成本汇总查询 |
| `agent/engine.rs` | 确保 tokens/cost 持续写入 |

### 数据流

```text
实时事件流
  -> Zustand agent-store
  -> Workspace UI

历史回放
  -> get_agent_events / list_agent_events
  -> hydrate store
  -> Workspace UI
```

### 与其他模块交互

- **← FM-02**: 接收多 Agent 执行态与状态变更
- **← FM-03**: 读取事件落库与验证结果
- **→ FM-06**: 为运行时介入提供上下文入口点

### 现有代码说明

当前 `WorkspaceView.tsx`（197 行）已实现：
- 单文本输入 + "Run Agent" 按钮
- Agent 标签页切换
- 实时事件流展示（`agent-event` + `agent-stream` 订阅）
- 文本流式显示

本模块需要**重构而非重写**此文件，重构方向：
- 拆分为组件（`AgentStreamList`, `AgentStreamCard`, `AgentTimeline`, `CostSummaryBar`）
- 输入栏移至 FM-01 的 MissionsView，WorkspaceView 变为纯展示
- 增加 Mission 过滤和历史回放能力

现有前端事件订阅：`src/ipc/events.ts` → `onAgentEvent()`, `onAgentStream()`
