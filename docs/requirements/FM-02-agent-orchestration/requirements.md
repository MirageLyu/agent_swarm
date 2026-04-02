# FM-02: Multi-Agent Orchestration

> 版本: v1.0 | 日期: 2026-04-01  
> 优先级: P1 | 预估周期: 5-7 天  
> 依赖: FM-01 | 被依赖: FM-04, FM-05

---

## IR — Initial Requirements

### IR-01: 用户故事

**US-01**: 作为开发者，我希望在确认 Mission 后，系统能自动把多个 ready task 分配给多个 Agent 并并行执行，这样我能真正获得 swarm 式产能。

**US-02**: 作为开发者，我希望每个 Agent 在独立的 git worktree 中工作，这样它们不会互相覆盖文件。

**US-03**: 作为开发者，我希望调度器能根据依赖关系自动启动后继任务，这样我不需要手动盯着 DAG 推进。

### IR-02: 业务价值

- 从“单 Agent 演示”升级为“多 Agent 协同”，验证产品核心叙事
- worktree 隔离是后续 Code Review、竞赛模式、回滚能力的基础
- 自动调度使 DAG 从静态图升级为可执行计划

### IR-03: 高层验收标准

1. 已确认的 Mission 中，所有 `ready` task 能被调度器自动拾取
2. 至少支持 2-3 个 Agent 并行执行
3. 每个 Agent 使用独立 worktree 路径和独立分支
4. 上游 task 完成后，下游 task 自动从 `pending` 转为 `ready`
5. Agent 失败不导致整个 Mission 崩溃，失败状态可见

---

## SR — Software Requirements

### 功能需求

#### FR-01: 调度器主循环

- **FR-01.1**: 后端新增 `Scheduler`，仅在用户显式调用 `start_mission_execution` 后启动，**不随应用启动自动运行**
- **FR-01.2**: 启动后周期性扫描 `tasks.status = 'ready'` 的任务，扫描周期默认 1000ms，可配置
- **FR-01.3**: 调度器最多同时启动 `max_concurrent_agents` 个 Agent
- **FR-01.4**: 调度前需原子性地将 task 从 `ready` 更新为 `running`，避免重复抢占
- **FR-01.5**: 调度器仅分配 `mission.status in ('planned', 'running')` 的任务
- **FR-01.6**: 当 Mission 内所有 task 达到终态后，调度器自动停止该 Mission 的轮询

#### FR-02: Worktree 生命周期管理

- **FR-02.1**: 每个被调度的 task 启动前创建独立 worktree
- **FR-02.2**: worktree 路径规则为 `<repo>/.worktrees/<agent_id>`
- **FR-02.3**: 分支命名规则为 `agent/<agent_id>`
- **FR-02.4**: Agent 完成后保留 worktree，供 FM-05 读取 diff
- **FR-02.5**: Agent 失败或取消时 worktree 也保留，便于排查
- **FR-02.6**: 提供显式清理接口，后续由用户或系统统一回收

#### FR-03: Agent 实例创建

- **FR-03.1**: 调度器为每个 task 创建一条 `agents` 记录
- **FR-03.2**: `agents` 需记录 `id`, `task_id`, `status`, `worktree_path`, `current_step`
- **FR-03.3**: 调度器调用 `AgentEngine::run()` 时传入 task 描述、workspace_path、agent_id、model
- **FR-03.4**: 启动 Agent 后立即向前端发送 `agent-started` 事件

#### FR-04: 依赖推进

- **FR-04.1**: 当 task 完成后，系统重新检查其所有下游任务
- **FR-04.2**: 若某下游任务的全部依赖都为 `completed`，则将其状态更新为 `ready`
- **FR-04.3**: 若上游任务失败，则下游任务保持 `pending`，并在 UI 标记“blocked”
- **FR-04.4**: Mission 终态策略：
  - 当全部 task 为 `completed` → Mission 为 `completed`
  - 当存在 `failed` 且无 `running/ready` → Mission 为 `failed`（但已 completed 的 task 成果保留，不回滚）
  - 终态判定时机：每个 task 达到终态后触发一次检查

#### FR-05: Mission 执行控制

- **FR-05.1**: 新增 `start_mission_execution` command，参数 `{ mission_id, repo_path }`
- **FR-05.2**: 新增 `get_scheduler_status` command，返回当前活跃 Agent 数、排队任务数、阻塞任务数
- **FR-05.3**: 若 `repo_path` 不是 git repo，则拒绝启动并返回明确错误
- **FR-05.4**: 若 `.worktrees` 目录不存在，系统自动创建
- **FR-05.5**: `repo_path` 由前端传入，Phase 1 阶段在 MissionsView 的 `Confirm & Start` 弹窗中让用户选择本地仓库路径（使用 Tauri 的 `dialog.open` API 选择目录）

> **注意**：当前代码中 `run_agent` 使用 `request.workspace_path`（前端传入 `/tmp/miragenty-workspace` 硬编码）。FM-02 必须消除此硬编码，改为用户选择的真实仓库路径。

### 非功能需求

- **NFR-01**: 调度器在 50 个任务内的调度延迟 ≤ 2 秒
- **NFR-02**: 同一 task 绝不能被两个 Agent 同时执行
- **NFR-03**: worktree 创建失败需可恢复，不得污染主工作区
- **NFR-04**: 调度器重启后应能从数据库恢复执行态，而不是丢失上下文

### 接口需求

新增 Tauri Commands：

| Command | 参数 | 返回 | 说明 |
|---------|------|------|------|
| `start_mission_execution` | `{ mission_id }` | `()` | 启动 Mission 调度 |
| `get_scheduler_status` | `—` | `{ active_agents, ready_tasks, blocked_tasks }` | 查询调度器状态 |
| `list_agents_by_mission` | `{ mission_id }` | `AgentInfo[]` | 查看 Mission 下 Agent 列表 |

新增事件：

| Event | Payload | 说明 |
|------|---------|------|
| `agent-started` | `{ agent_id, task_id, worktree_path }` | Agent 已启动 |
| `task-status-changed` | `{ task_id, from, to }` | Task 状态变更 |
| `mission-status-changed` | `{ mission_id, from, to }` | Mission 状态变更 |

### 数据需求

需复用并约束现有表字段：

- `tasks.assigned_agent_id` 在调度时写入
- `agents.worktree_path` 必填
- `missions.status` 允许 `planned/running/completed/failed`
- 新增 blocked 判定逻辑可先不落库，通过前端根据依赖状态推导

---

## AR — Architecture Requirements

### 组件设计

```text
Mission Confirmed
      │
      ▼
 Scheduler Loop
      │
      ├─ query ready tasks
      ├─ create agent record
      ├─ create worktree
      ├─ spawn AgentEngine task
      └─ update task/mission state
                 │
                 ▼
          Dependency Resolver
```

### 后端模块变更

| 文件 | 变更 |
|------|------|
| `src-tauri/src/agent/scheduler.rs`（新） | 调度器主循环、并发控制、状态推进 |
| `src-tauri/src/agent/mod.rs` | 导出 `scheduler` |
| `src-tauri/src/commands/agent.rs` | 新增 Mission 级执行命令 |
| `src-tauri/src/git/worktree.rs` | 接入真实 repo 路径与保留策略 |
| `src-tauri/src/db/*` | 增加任务状态流转相关查询/更新函数 |

### 前端模块变更

| 文件 | 变更 |
|------|------|
| `src/views/MissionsView.tsx` | `Confirm & Start` 改为真正启动 Mission |
| `src/views/WorkspaceView.tsx` | 展示按 Mission 聚合的 Agent 列表 |
| `src/stores/task-store.ts` | 支持 task 状态实时更新 |
| `src/stores/agent-store.ts` | 支持调度器驱动的 Agent 增删改 |

### 状态机

#### Task 状态机

`pending -> ready -> running -> completed`

异常分支：

`running -> failed`

`ready/running -> cancelled`

#### Mission 状态机

`draft -> planned -> running -> completed`

异常分支：

`running -> failed`

### 时序图

```text
用户确认 Mission
  -> 前端调用 start_mission_execution
  -> Scheduler 扫描 ready tasks
  -> create worktree + agent row
  -> AgentEngine.run
  -> task running
  -> task completed
  -> Dependency Resolver 更新下游为 ready
  -> Scheduler 继续分配
```

### 与其他模块交互

- **← FM-01**: 读取 Mission、Task、Dependency 数据作为调度输入
- **→ FM-04**: 向活动流推送 Agent 启停和 task 状态变化
- **→ FM-05**: 提供 `worktree_path` 与 diff 数据源
- **← FM-03**: 复用 Agent 取消、checkpoint、schema 验证能力

### 现有代码关键入口

| 说明 | 文件路径 |
|------|---------|
| 现有 Agent 引擎 | `src-tauri/src/agent/engine.rs`（187 行） |
| 现有 run_agent command | `src-tauri/src/commands/agent.rs` |
| Git worktree 管理 | `src-tauri/src/git/worktree.rs` |
| 工具框架 | `src-tauri/src/tools/executor.rs`, `definitions.rs` |
| 前端 WorkspaceView | `src/views/WorkspaceView.tsx` |
| 前端 agent store | `src/stores/agent-store.ts` |
| 前端 task store | `src/stores/task-store.ts` |
| 配置（并发数） | `src-tauri/src/commands/config.rs` → `max_concurrent_agents` |
