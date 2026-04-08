# FM-09: UI Polish — 原型设计对齐

> 版本: v1.0 | 日期: 2026-04-07  
> 优先级: P0 | 预估周期: 5-7 天  
> 依赖: FM-01 ~ FM-08（全部已完成） | 被依赖: Phase 2 全部模块

---

## IR — Initial Requirements

### IR-01: 用户故事

**US-01**: 作为开发者，我希望在标题栏始终看到当前 Swarm 的关键指标（Agent 在线数、运行时长、累计成本），这样我无需切换页面就能掌握全局态势。

**US-02**: 作为开发者，我希望 Agent 的实时输出以终端风格展示（彩色行、闪烁光标、Grid 并排布局），这样我能直观地"看到"Agent 在工作。

**US-03**: 作为开发者，我希望 Task DAG 的节点可以拖拽重新定位、点击查看详细信息面板、边线能反映任务运行状态，这样 DAG 不只是静态图而是活的交互界面。

**US-04**: 作为开发者，我希望在侧边栏看到所有 Agent 的实时状态列表，并能通过 Command Palette (⌘K) 快速触发常用操作。

**US-05**: 作为开发者，我希望在 Code Review 界面有过滤标签、汇总统计和批量操作按钮，这样审查效率更高。

### IR-02: 业务价值

- 原型设计是产品 Demo 的视觉基准，当前实现与原型差距显著
- TopBar 实时指标和 Agent Stream Grid 模式是用户第一印象的核心
- DAG 交互丰富度直接影响"可操控性"这一差异化卖点
- Phase 2 的 Evaluator 注释、Mission Report、Dashboard 都依赖本模块建立的 UI 骨架

### IR-03: 高层验收标准

1. TopBar 实时显示 Agent 在线数、运行时长、Mission 成本
2. Agent Stream 支持 Grid 模式（2×2 终端面板）+ 终端风格彩色输出
3. Task DAG 节点可拖拽、有右侧详情面板、边线按状态着色
4. Sidebar 底部显示 Agent 状态列表
5. Command Palette (⌘K) 可用
6. Code Review 有过滤标签、汇总栏、Approve All 按钮
7. Settings 中 Provider/Model/Max Agents 可编辑

---

## SR — Software Requirements

### 功能需求

#### FR-01: TopBar 实时指标

- **FR-01.1**: Titlebar 右侧显示三组 Badge：Agent 在线数 `N/M`、当前 Mission 运行时长 `Xm Ys`、累计成本 `$X.XX/$Budget`
- **FR-01.2**: 指标每 3 秒刷新，数据来源：`get_scheduler_status`（Agent 数）、`get_mission_cost_summary`（成本）、前端计时器（时长）
- **FR-01.3**: 无活跃 Mission 时隐藏指标区域
- **FR-01.4**: 成本超过预算 50% 变橙色，超过 80% 变红色（预算值暂取 config 中可扩展字段，MVP 阶段硬编码 $30）

#### FR-02: Agent Stream — Grid 模式

- **FR-02.1**: WorkspaceView 新增 Grid/List/Focus 三模式 Segmented Control
- **FR-02.2**: Grid 模式下以 2×2 网格排列 Agent 面板（≤2 个时自适应 1 列），每个面板含头部（Agent 名 + 状态 + 任务名）和终端输出区
- **FR-02.3**: Grid 面板头部有 Focus 按钮（→ Focus 模式）和三点菜单（Send Note / Pause / Kill）
- **FR-02.4**: 超过 4 个 Agent 时，Grid 区域可滚动（2 列 × N 行）

#### FR-03: 终端风格输出

- **FR-03.1**: Agent 输出区使用深色背景 + 等宽字体（SF Mono / Menlo）
- **FR-03.2**: 事件行按 kind 着色：`llm_call`=蓝、`tool_use`=青、`tool_result`/`message`=白/灰、`error`=红、`checkpoint`=暗灰、`status_change`=橙、`note_applied`=黄底
- **FR-03.3**: 运行中的 Agent 终端末尾显示闪烁光标 `█`（CSS animation blink 1s）
- **FR-03.4**: 新行入场带 `translateY(3px)` + `fadeIn` 微动画（duration 150ms）
- **FR-03.5**: Grid 和 Focus 模式均使用终端风格；List 模式保持当前卡片风格不变

#### FR-04: Per-Agent 操作菜单

- **FR-04.1**: Grid/Focus 模式下每个 Agent 面板头部提供三点菜单（Radix DropdownMenu）
- **FR-04.2**: 菜单项：Send Note（弹出便签输入框）、Pause（调用 `stop_agent`）、Kill + Restart（调用 `stop_agent`，确认对话框）
- **FR-04.3**: Agent 非 running 状态时 Pause 和 Kill 菜单项禁用

#### FR-05: Task DAG 增强

- **FR-05.1**: 节点支持鼠标拖拽重新定位（mousedown 启动、mousemove 更新位置、mouseup 结束），拖拽时实时重绘 SVG 连线
- **FR-05.2**: 拖拽后的位置保存在前端 state 中（不持久化到后端），Auto Layout 按钮可一键恢复自动布局
- **FR-05.3**: DAG 右侧增加详情面板（280px 宽，可折叠）：选中节点后显示任务描述、状态 Badge、Agent 名、费用、上游/下游依赖列表
- **FR-05.4**: 未选中节点时面板显示空状态图标 + "点击节点查看详情"
- **FR-05.5**: SVG 边按状态着色：completed=绿实线（#34C759）、running=蓝色（#007AFF）+ `stroke-dasharray` 流动动画、pending=灰虚线（#8E8E93）
- **FR-05.6**: DAG 底部增加汇总栏：显示 "N tasks: X completed · Y running · Z pending" + 费用进度条
- **FR-05.7**: DAG 节点信息扩展：显示 Agent 名（如有）、任务类型标签（颜色区分）、费用

#### FR-06: Sidebar Agent 列表

- **FR-06.1**: Sidebar 底部新增 "Agents" 分组，列出当前 Mission 关联的所有 Agent
- **FR-06.2**: 每个 Agent 项显示：状态圆点（running=绿脉冲、waiting=橙、idle=灰、failed=红、completed=绿静态）+ Agent 名 + 当前任务名摘要
- **FR-06.3**: 点击 Agent 项 → 切换到 Workspace Focus 模式并聚焦该 Agent
- **FR-06.4**: Agent 列表数据通过监听 `agent-started` / `task-status-changed` / `mission-status-changed` 事件实时更新

#### FR-07: Command Palette

- **FR-07.1**: `⌘K`（macOS）/ `Ctrl+K` 打开 Command Palette 浮层（居中 560px 宽，backdrop blur）
- **FR-07.2**: 顶部搜索输入框，支持模糊匹配命令名
- **FR-07.3**: 预置命令列表：New Mission (⌘N) / Send Breadcrumb (⌘B) / Switch View / Toggle Theme (⌘⇧T)
- **FR-07.4**: `Escape` 或点击外部关闭，带缩放 + 透明度过渡动画（200ms）
- **FR-07.5**: 选中命令后执行对应操作（切换视图、打开对话框等）

#### FR-08: Code Review 增强

- **FR-08.1**: ReviewView 顶部增加过滤标签页：All / Needs Review / Approved，每项显示计数 Badge
- **FR-08.2**: 文件列表按 Agent（或 Task）分组，每组有 Agent/Task 名头
- **FR-08.3**: Diff 面板上方增加审查摘要栏："N files changed · X approved · Y needs review"
- **FR-08.4**: 顶栏增加 "Approve All" 按钮（批量标记所有 Agent 为 approved）和 "Merge All" 按钮（预留，Phase 1 仅 toast 提示）

#### FR-09: Settings 可编辑

- **FR-09.1**: Provider（文本输入）、Base URL（文本输入）、Default Model（文本输入）、Max Concurrent Agents（数字输入）全部可编辑
- **FR-09.2**: 编辑后点击 Save 调用 `update_config` 保存
- **FR-09.3**: 保存成功后显示 2 秒成功提示

### 非功能需求

- **NFR-01**: Grid 模式下 4 个 Agent 同时推流时帧率 ≥ 30fps
- **NFR-02**: 节点拖拽无明显延迟（< 16ms 响应）
- **NFR-03**: Command Palette 打开/关闭 < 200ms
- **NFR-04**: 终端输出区支持 1000 行不卡顿（超出自动裁剪旧行）

### 接口需求

无新增 Tauri Command。所有数据来源复用已有命令：

| 已有 Command | 用途 |
|-------------|------|
| `get_scheduler_status` | TopBar Agent 数 |
| `get_mission_cost_summary` | TopBar 成本 / DAG 汇总 |
| `list_agents_by_mission` | Sidebar Agent 列表 / DAG 详情 |
| `get_mission_detail` | DAG 节点扩展信息 |
| `stop_agent` | Per-Agent 菜单 Pause/Kill |
| `inject_agent_note` | Per-Agent 菜单 Send Note |
| `submit_review_action` | Approve All 批量操作 |
| `update_config` | Settings 编辑 |

### 数据需求

无 Schema 变更。所有数据已在 FM-01 ~ FM-08 的迁移中就位。

---

## AR — Architecture Requirements

### 前端组件变更

#### 新增组件

| 组件 | 路径 | 职责 |
|------|------|------|
| `TopBarMetrics` | `src/components/TopBarMetrics.tsx` | Titlebar 内嵌的实时指标 Badge 组 |
| `AgentTerminalPane` | `src/components/workspace/AgentTerminalPane.tsx` | 终端风格单 Agent 输出面板（Grid/Focus 共用） |
| `AgentGridView` | `src/components/workspace/AgentGridView.tsx` | 2×2 Grid 布局容器 |
| `AgentPaneMenu` | `src/components/workspace/AgentPaneMenu.tsx` | Per-Agent 三点操作菜单 |
| `TaskDetailPanel` | `src/components/mission/TaskDetailPanel.tsx` | DAG 右侧任务详情面板 |
| `DagSummaryBar` | `src/components/mission/DagSummaryBar.tsx` | DAG 底部汇总栏 |
| `SidebarAgentList` | `src/components/SidebarAgentList.tsx` | Sidebar Agent 状态列表 |
| `CommandPalette` | `src/components/CommandPalette.tsx` | ⌘K 命令面板浮层 |
| `ReviewFilterBar` | `src/components/review/ReviewFilterBar.tsx` | Review 过滤标签页 + 汇总 |

#### 修改组件

| 组件 | 变更说明 |
|------|---------|
| `Titlebar.tsx` | 集成 `TopBarMetrics`，⌘K 按钮绑定 Command Palette |
| `Sidebar.tsx` | 底部增加 `SidebarAgentList` |
| `WorkspaceView.tsx` | 增加 Grid/List/Focus 三模式切换，引入 `AgentGridView` |
| `AgentStreamCard.tsx` | 保持 List 模式样式不变 |
| `AgentTimeline.tsx` | Focus 模式内嵌 `AgentTerminalPane` 替代当前事件列表 |
| `DAGViewport.tsx` | 节点拖拽逻辑 + 边状态着色 |
| `TaskDAG.tsx` | 集成 `TaskDetailPanel` + `DagSummaryBar` |
| `TaskNode.tsx` | 扩展显示 Agent 名 / 费用 / 类型标签 |
| `TaskEdge.tsx` | 按 status 参数渲染不同颜色/动画 |
| `MissionsView.tsx` | 布局调整容纳 `TaskDetailPanel` |
| `ReviewView.tsx` | 集成 `ReviewFilterBar`、增加 Approve All / Merge All |
| `SettingsView.tsx` | 所有配置字段改为可编辑 + Save 按钮 |

### 前端状态变更

| Store | 变更 |
|-------|------|
| `ui-store` | 新增 `workspaceMode: 'grid' \| 'list' \| 'focus'`、`commandPaletteOpen: boolean`、`dagSelectedTaskId: string \| null` |
| `agent-store` | 新增 `sidebarAgents` 数组（轻量级，仅 id/name/status/taskTitle） |

### CSS 新增

| 文件 | 说明 |
|------|------|
| `AgentTerminalPane.module.css` | 终端深色背景 + 等宽字体 + 行类型色彩 + 光标闪烁 |
| `AgentGridView.module.css` | 2×2 网格布局 + 响应式 |
| `CommandPalette.module.css` | 浮层 + backdrop blur + 搜索框 + 命令列表 |
| `TaskDetailPanel.module.css` | 右侧面板折叠/展开 + 信息表 |
| `TopBarMetrics.module.css` | Badge 组排列 + 成本色彩阈值 |
| `DagSummaryBar.module.css` | 底部栏 + 进度条 |
| `ReviewFilterBar.module.css` | 标签页 + 计数 Badge |

### 与原型设计的对应关系

| 原型文件 | 对应需求 |
|---------|---------|
| `01-commander-shell.html` TopBar | FR-01 |
| `01-commander-shell.html` Sidebar Agents | FR-06 |
| `01-commander-shell.html` Command Palette | FR-07 |
| `03-task-dag.html` 节点拖拽 + 详情面板 + 边着色 + 汇总栏 | FR-05 |
| `04-agent-stream.html` Grid 模式 + 终端风格 + 菜单 | FR-02, FR-03, FR-04 |
| `05-code-review.html` 过滤 + 汇总 + 批量操作 | FR-08 |
| SettingsView 可编辑 | FR-09 |

### 现有代码关键入口

| 文件 | 当前行数 | 修改方向 |
|------|:---:|------|
| `src/components/Titlebar.tsx` | 83 | 右侧区域增加 `TopBarMetrics`，⌘K 按钮绑定事件 |
| `src/components/Sidebar.tsx` | 127 | 底部增加 `SidebarAgentList`（需要折叠/展开控制） |
| `src/views/WorkspaceView.tsx` | 365 | 增加视图模式切换，Grid 分支引入 `AgentGridView` |
| `src/components/mission/DAGViewport.tsx` | 253 | 增加拖拽事件处理（mousedown/move/up） |
| `src/components/mission/TaskEdge.tsx` | 21 | 接收 `status` prop，条件渲染色彩/动画 |
| `src/components/mission/TaskNode.tsx` | 109 | 扩展 props 显示 agent/cost/type |
| `src/views/MissionsView.tsx` | 380 | 布局调整为三栏（list + DAG + detail panel） |
| `src/views/ReviewView.tsx` | 203 | 顶部增加 `ReviewFilterBar` + 底部 Approve All |
| `src/views/SettingsView.tsx` | 94 | 字段改为受控 Input + Save 逻辑 |
| `src/ipc/commands.ts` | 342 | 无新增，但需确认 `updateConfig` 支持所有字段 |

### 实现建议顺序

```
Sprint A (Day 1-3): 视觉冲击力优先
  ├── FR-03: 终端风格输出（AgentTerminalPane）
  ├── FR-02: Grid 模式
  ├── FR-01: TopBar 实时指标
  └── FR-05.5: DAG 边状态着色

Sprint B (Day 3-5): DAG 交互
  ├── FR-05.1~05.2: 节点拖拽
  ├── FR-05.3~05.4: 右侧详情面板
  ├── FR-05.6~05.7: 汇总栏 + 节点扩展
  └── FR-04: Per-Agent 操作菜单

Sprint C (Day 5-7): 辅助功能
  ├── FR-06: Sidebar Agent 列表
  ├── FR-07: Command Palette
  ├── FR-08: Review 增强
  └── FR-09: Settings 可编辑
```
