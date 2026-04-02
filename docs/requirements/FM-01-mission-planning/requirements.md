# FM-01: Mission Planning & Task DAG

> 版本: v1.0 | 日期: 2026-04-01  
> 优先级: P0 | 预估周期: 5-7 天  
> 依赖: 无（入口模块） | 被依赖: FM-02, FM-04

---

## IR — Initial Requirements（初始需求）

### IR-01: 用户故事

**US-01**: 作为开发者，我希望在对话面板中用自然语言描述一个开发任务，系统能自动将其拆解为多个可执行的子任务，这样我不需要手动分解工作。

**US-02**: 作为开发者，我希望看到任务之间的依赖关系（DAG 图），并能手动调整任务顺序、删除不需要的任务、或添加遗漏的任务，这样我对执行计划有完全控制权。

**US-03**: 作为开发者，我希望在确认任务计划后一键启动执行，这样从需求到执行的流程是连贯的。

### IR-02: 业务价值

- 用户流程的**入口**——所有 Agent 工作始于任务分解
- 验证 Planner Agent 是否能产出**合理的任务拆解**
- 任务 DAG 是后续 FM-02（调度器）的数据源
- 可视化 DAG 是产品 Demo 的关键展示页面

### IR-03: 高层验收标准

1. 用户输入一句话需求 → 系统在 15 秒内返回结构化的任务列表
2. 任务列表包含：标题、描述、预估复杂度、依赖关系
3. 任务以 DAG 形式可视化展示，节点可交互
4. 用户可以编辑 DAG（删除/添加/重排）
5. 确认后任务写入数据库，状态变为"等待执行"

---

## SR — Software Requirements（软件需求）

### 功能需求

#### FR-01: 需求输入面板

- **FR-01.1**: MissionsView 顶部提供文本输入区域，支持多行文本输入（最大 2000 字符）
- **FR-01.2**: 输入区域下方有"Plan Mission"按钮，点击后触发 Planner Agent 调用
- **FR-01.3**: 按钮点击后进入 loading 状态，显示"Planning..."动画，禁止重复提交
- **FR-01.4**: 支持 `Cmd+Enter` 快捷键提交

#### FR-02: Planner Agent 调用

- **FR-02.1**: 后端提供 `plan_mission` Tauri command，接收 `{ description: string }` 参数
- **FR-02.2**: 该 command 调用 LLM（使用当前配置的 provider/model），system prompt 指导 LLM 以 JSON 格式输出任务列表
- **FR-02.3**: LLM 输出的 JSON 结构：
  ```json
  {
    "mission_title": "string",
    "tasks": [
      {
        "id": "T1",
        "title": "string",
        "description": "string",
        "complexity": "low | medium | high",
        "depends_on": ["T0"]
      }
    ]
  }
  ```
- **FR-02.4**: 后端解析 LLM 输出，校验 JSON 格式和依赖引用的合法性（无循环依赖、无引用不存在的 task ID）
- **FR-02.5**: 校验失败时自动重试一次（附上错误信息让 LLM 修正），两次失败则返回错误给前端
- **FR-02.6**: 成功后创建 Mission 记录和关联的 Task 记录，写入 SQLite

#### FR-03: Task DAG 可视化

- **FR-03.1**: MissionsView 展示当前 Mission 的 Task DAG，采用从左到右布局（源节点在左，汇节点在右）
- **FR-03.2**: 每个节点显示：任务标题、complexity 标签（颜色区分）、状态指示器（待执行/进行中/完成/失败）
- **FR-03.3**: 节点之间的依赖用带箭头的连线表示
- **FR-03.4**: 支持鼠标悬停节点显示 tooltip（完整描述、依赖列表）
- **FR-03.5**: DAG 布局使用简单的分层算法（按依赖深度分层），无需复杂的图布局库
- **FR-03.6**: Phase 1 阶段使用 SVG 渲染（不引入 D3.js 等重量级库），保持轻量

#### FR-04: DAG 编辑

- **FR-04.1**: 点击节点弹出操作菜单：查看详情 / 编辑 / 删除 / 添加依赖
- **FR-04.2**: "编辑"支持修改任务标题和描述
- **FR-04.3**: "删除"移除任务节点和所有关联的依赖边，级联更新受影响的下游任务
- **FR-04.4**: "添加任务"按钮在 DAG 区域外提供，点击后弹出表单（标题、描述、依赖选择）
- **FR-04.5**: 所有编辑操作实时反映在 DAG 可视化上
- **FR-04.6**: 每次编辑操作通过对应的单条 command（`update_task` / `delete_task` / `add_task`）即时同步回后端，采用乐观更新策略（前端先改，后台同步，失败时回滚）

#### FR-05: Mission 确认与启动

- **FR-05.1**: DAG 下方有"Confirm & Start"按钮，仅在任务列表非空时可用
- **FR-05.2**: 点击后将 Mission 状态更新为 `planned`，所有无依赖的 Task 状态更新为 `ready`
- **FR-05.3**: 触发后端事件通知 FM-02 调度器（预留接口，Phase 1 先手动触发单个 Agent）
- **FR-05.4**: UI 自动跳转到 WorkspaceView 展示 Agent 活动

#### FR-06: Mission 列表

- **FR-06.1**: MissionsView 左侧面板展示历史 Mission 列表，按创建时间倒序
- **FR-06.2**: 每项显示：标题、状态 Badge、创建时间、任务完成进度（如 3/6）
- **FR-06.3**: 点击切换当前查看的 Mission
- **FR-06.4**: 支持删除未启动的 Mission

### 非功能需求

- **NFR-01**: Planner LLM 调用响应时间 ≤ 15 秒（含网络延迟），超时显示友好提示
- **NFR-02**: DAG 渲染在 50 个节点以内保持流畅（≥ 30fps 交互）
- **NFR-03**: 任务数据在 Mission 创建后立即持久化，应用崩溃不丢失
- **NFR-04**: DAG 编辑操作响应时间 ≤ 100ms（乐观更新，后台同步）

### 接口需求

新增 Tauri Commands：

| Command | 参数 | 返回 | 说明 |
|---------|------|------|------|
| `plan_mission` | `PlanMissionRequest { description }` | `PlanMissionResponse { mission_id, tasks[] }` | Planner Agent 分解任务 |
| `get_mission_detail` | `{ mission_id }` | `MissionDetail { mission, tasks[], dependencies[] }` | 获取 Mission 完整信息 |
| `update_task` | `UpdateTaskRequest { task_id, title?, description?, status? }` | `()` | 更新单个 Task |
| `delete_task` | `{ task_id }` | `()` | 删除 Task + 关联依赖 |
| `add_task` | `AddTaskRequest { mission_id, title, description, complexity, depends_on[] }` | `TaskInfo` | 新增 Task |
| `confirm_mission` | `{ mission_id }` | `()` | 确认 Mission，更新状态 |

### 数据需求

#### 现有 schema 与需求文档的差异（必须在开发前对齐）

当前 SQLite schema（`db/migrations.rs` 001_initial）中的状态枚举与本文档不一致，需要新增迁移 `002_status_alignment`：

| 表 | 当前 CHECK 约束 | 本文档要求 | 需变更 |
|---|---|---|---|
| `missions.status` | `planning, executing, completed, failed` | `draft, planned, running, completed, failed` | **是** |
| `tasks.status` | `pending, queued, running, completed, failed, cancelled` | `pending, ready, running, completed, failed, cancelled` | **是**（`queued` → `ready`） |
| `agents.status` | `idle, planning, executing, waiting_checkpoint, completed, failed` | `idle, running, completed, failed, cancelled` | **是** |

同时 `tasks` 表当前缺少 `complexity` 字段，需新增：
- `complexity TEXT NOT NULL DEFAULT 'medium' CHECK (complexity IN ('low', 'medium', 'high'))`

`task_dependencies` 表已满足 DAG 关系存储需求，无需变更。

#### 迁移策略

由于应用处于早期开发阶段，可以直接修改 `001_initial` 迁移内容（清空数据重建），或新增 `002_status_alignment` 迁移执行 ALTER TABLE。开发 Agent 可自行决定最合适的方式。

---

## AR — Architecture Requirements（架构需求）

### 组件设计

```
┌──────────────────────────────────────────────────────────┐
│                     MissionsView                          │
│  ┌──────────────┐  ┌──────────────────────────────────┐  │
│  │ MissionList   │  │ MissionDetail                     │  │
│  │ (左侧面板)    │  │  ┌────────────────────────────┐  │  │
│  │               │  │  │ PlanInput                   │  │  │
│  │  mission-1 ►  │  │  │ (需求输入 + Plan 按钮)      │  │  │
│  │  mission-2    │  │  └────────────────────────────┘  │  │
│  │  mission-3    │  │  ┌────────────────────────────┐  │  │
│  │               │  │  │ TaskDAG                     │  │  │
│  │               │  │  │ (SVG DAG 可视化)             │  │  │
│  │               │  │  │ + 节点交互 + 编辑           │  │  │
│  │               │  │  └────────────────────────────┘  │  │
│  │               │  │  ┌────────────────────────────┐  │  │
│  │               │  │  │ MissionActions              │  │  │
│  │               │  │  │ (Confirm & Start 按钮)      │  │  │
│  │               │  │  └────────────────────────────┘  │  │
│  └──────────────┘  └──────────────────────────────────┘  │
└──────────────────────────────────────────────────────────┘
```

### 前端组件清单

| 组件 | 路径 | 职责 |
|------|------|------|
| `MissionsView` | `src/views/MissionsView.tsx` | 容器组件，管理布局和状态 |
| `MissionList` | `src/components/mission/MissionList.tsx` | 左侧 Mission 列表 |
| `MissionListItem` | `src/components/mission/MissionListItem.tsx` | 单个 Mission 条目 |
| `PlanInput` | `src/components/mission/PlanInput.tsx` | 需求输入 + Plan 按钮 |
| `TaskDAG` | `src/components/mission/TaskDAG.tsx` | SVG DAG 渲染与交互 |
| `TaskNode` | `src/components/mission/TaskNode.tsx` | DAG 中的单个任务节点 |
| `TaskEdge` | `src/components/mission/TaskEdge.tsx` | DAG 中的依赖连线 |
| `TaskEditDialog` | `src/components/mission/TaskEditDialog.tsx` | 任务编辑弹窗（Radix Dialog） |

### 后端模块变更

| 文件 | 变更 |
|------|------|
| `commands/mission.rs` | 新增 `plan_mission`, `get_mission_detail`, `update_task`, `delete_task`, `add_task`, `confirm_mission` |
| `commands/mod.rs` | 注册新 commands |
| `lib.rs` | 注册新 commands 到 invoke_handler |
| `agent/planner.rs`（新文件） | Planner Agent 逻辑：构造 prompt、调用 LLM、解析+校验 JSON |

### Planner Agent System Prompt 设计

```
You are a task planner for a software development project. Given a high-level
requirement description, decompose it into concrete, independently executable
sub-tasks.

Output ONLY a valid JSON object with this structure:
{
  "mission_title": "concise title for the overall mission",
  "tasks": [
    {
      "id": "T1",
      "title": "short task title",
      "description": "detailed description of what this task should accomplish",
      "complexity": "low|medium|high",
      "depends_on": []
    }
  ]
}

Rules:
- Each task should be completable by a single AI agent
- IDs must be sequential: T1, T2, T3...
- depends_on references must be valid task IDs defined earlier
- No circular dependencies
- Aim for 3-10 tasks depending on complexity
- Distinguish frontend/backend/test tasks where applicable
- Order dependencies logically (data model before API, API before UI)
```

### DAG 布局算法

使用 Sugiyama 简化版（分层布局）：

1. **拓扑排序**：将 tasks 按依赖关系拓扑排序
2. **层级分配**：无依赖的 task 放第 0 层，依赖第 0 层的放第 1 层，以此类推
3. **层内排序**：同层 task 按 ID 排序
4. **坐标计算**：每层 x 坐标递增，层内 y 坐标均匀分布
5. **边路由**：依赖线从源节点右侧中点 → 目标节点左侧中点，直线连接

不需要引入 dagre/elkjs 等库，50 节点以内自行计算足够。

### 时序图

```
用户                 前端 MissionsView       后端 plan_mission       LLM Provider
 │                        │                        │                      │
 │── 输入需求 + 点击 Plan ─►│                        │                      │
 │                        │── invoke plan_mission ──►│                      │
 │                        │                        │── chat(planner prompt)─►│
 │                        │                        │◄── JSON response ──────│
 │                        │                        │── 校验 JSON            │
 │                        │                        │── 写入 missions + tasks│
 │                        │◄── PlanMissionResponse ─│                      │
 │                        │── 渲染 TaskDAG          │                      │
 │◄── 展示 DAG ───────────│                        │                      │
 │                        │                        │                      │
 │── 编辑 DAG（增删改）────►│                        │                      │
 │                        │── invoke update_task ──►│                      │
 │                        │◄── success ─────────────│                      │
 │                        │                        │                      │
 │── 点击 Confirm & Start ►│                        │                      │
 │                        │── invoke confirm_mission►│                      │
 │                        │                        │── 更新状态 planned     │
 │                        │◄── success ─────────────│                      │
 │                        │── 跳转 WorkspaceView    │                      │
```

### 与其他模块的交互

- **→ FM-02**: `confirm_mission` 后，FM-02 的调度器轮询 `tasks` 表中 `status = ready` 的任务
- **→ FM-04**: Mission 的 tasks 状态变更通过 Tauri events 推送到 ActivityStream
- **← FM-02**: Agent 完成任务后更新 `tasks.status`，MissionsView 通过轮询或事件刷新 DAG 节点颜色

### 现有代码关键入口（供开发 Agent 快速定位）

| 说明 | 文件路径 |
|------|---------|
| 现有 MissionsView 占位组件 | `src/views/MissionsView.tsx` |
| 现有 mission CRUD command | `src-tauri/src/commands/mission.rs` |
| LLM Provider trait + 实现 | `src-tauri/src/llm/provider.rs`, `anthropic.rs`, `openai_compat.rs` |
| 配置系统（获取 provider/model/api_key） | `src-tauri/src/commands/config.rs` |
| Tauri command 注册入口 | `src-tauri/src/lib.rs` invoke_handler |
| SQLite 迁移 | `src-tauri/src/db/migrations.rs` |
| 前端 IPC 封装 | `src/ipc/commands.ts`, `src/ipc/events.ts` |
| 设计系统 CSS 变量 | `src/styles/variables.css` |
| UI 组件库 | `src/components/ui/Button.tsx`, `Input.tsx`, `Badge.tsx` |
| Radix UI Dialog | 已安装 `@radix-ui/react-dialog`（见 package.json） |
