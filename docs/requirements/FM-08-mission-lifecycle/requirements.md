# FM-08: Mission Lifecycle — 删除与重新执行

> 版本: v1.0 | 日期: 2026-04-02  
> 优先级: P2 | 预估周期: 2-3 天  
> 依赖: FM-01（Mission 数据模型）, FM-02（Scheduler + Worktree）  
> 被依赖: 无

---

## IR — Initial Requirements（初始需求）

### IR-01: 用户故事

**US-01**: 作为开发者，我希望能删除任何状态的历史 Mission（包括已完成和已失败的），这样我可以清理不再需要的实验和失败尝试，保持列表整洁。

**US-02**: 作为开发者，我希望能将一个已完成或已失败的 Mission 重新执行，这样我在修改了 LLM 配置或任务描述后，可以让 Agent 重新尝试，而无需重新规划整个 Mission。

**US-03**: 作为开发者，我希望重新执行时可以选择"全部重跑"或"仅重跑失败的任务"，这样我不用浪费时间和 tokens 重跑已经成功的部分。

### IR-02: 业务价值

- 当前只能删除 `draft` 状态的 Mission，历史数据无法清理
- 失败的 Mission 无法重试，用户只能重新创建和规划
- Mission 数量增多后列表变得混乱，影响使用效率
- 重新执行是 Agent 调优的核心循环：规划 → 执行 → 发现问题 → 调整 → 重新执行

### IR-03: 高层验收标准

1. 用户可以删除 `draft`、`planned`、`completed`、`failed` 状态的 Mission
2. 删除 `running` 状态的 Mission 前，必须先停止调度器
3. 删除时同步清理关联的数据库记录（tasks, agents, events, cost_records）
4. 删除时可选是否清理磁盘上的 worktree 目录
5. 用户可以对 `completed` 或 `failed` 的 Mission 发起"重新执行"
6. 重新执行提供两种模式：全部重跑、仅重跑失败
7. 重新执行前弹出确认对话框，提示将重置相关任务和 Agent 数据

---

## SR — Software Requirements（软件需求）

### 功能需求

#### FR-01: 扩展 Mission 删除

**现状**: `delete_mission` 仅允许删除 `draft` 状态。

**目标**: 支持删除所有非 `running` 状态的 Mission。

- FR-01.1: `draft`、`planned`、`completed`、`failed` 状态的 Mission 可直接删除
- FR-01.2: `running` 状态的 Mission 不可删除，返回错误提示用户先停止
- FR-01.3: 删除时级联清理所有关联数据：
  - `tasks`（已通过 `ON DELETE CASCADE` 处理）
  - `task_dependencies`（已通过 `ON DELETE CASCADE` 处理）
  - `agents` 表中 `task_id` 关联的记录
  - `agent_events` 表中相关记录（通过 agents CASCADE）
  - `cost_records` 表中相关记录（通过 agents CASCADE）
- FR-01.4: 可选清理磁盘 worktree 目录（参数 `clean_workspace: bool`）
  - 若 `clean_workspace = true`，删除 Mission 对应工作区目录下的 `.worktrees/` 内容
  - 需要知道 repo_path：新增 `missions.repo_path` 字段（或从最近一次执行记录中获取）

#### FR-02: 停止运行中的 Mission

**前提**: FM-02 Scheduler 已实现 `stop_mission`，但未暴露为前端可调用的 command。

- FR-02.1: 新增 `stop_mission_execution` Tauri command
- FR-02.2: 停止后 Mission 状态设为 `failed`，附带原因 `"user_cancelled"`
- FR-02.3: 所有 `running` 状态的任务重置为 `ready`
- FR-02.4: 前端 Mission 卡片在 `running` 状态下显示"停止"按钮

#### FR-03: 重新执行 — 全部重跑

- FR-03.1: 新增 `restart_mission` Tauri command，参数 `{ mission_id, mode: "full" | "failed_only" }`
- FR-03.2: `mode = "full"` 时：
  - 将所有任务状态重置为初始态（无依赖的 → `ready`，有未完成依赖的 → `pending`）
  - 清除所有任务的 `assigned_agent_id` 和 `completed_at`
  - 删除所有相关 agents 记录（及其 events、cost_records）
  - 清理 worktree 目录
  - Mission 状态设为 `planned`
- FR-03.3: 重置后用户需要再次选择工作区路径并点击 Start

#### FR-04: 重新执行 — 仅重跑失败

- FR-04.1: `mode = "failed_only"` 时：
  - 仅重置 `failed` 和 `cancelled` 状态的任务
  - 重置为 `ready`（若其上游依赖全部 completed）或 `pending`
  - 删除这些任务对应的 agents 记录（及其 events、cost_records）
  - 清理对应 worktree
  - 保留 `completed` 状态任务的全部数据和 commit 记录
  - Mission 状态设为 `planned`
- FR-04.2: 如果没有 `failed`/`cancelled` 的任务（全部 completed），返回提示无需重跑

#### FR-05: 前端 UI

- FR-05.1: Mission 列表项右键菜单 / 下拉操作菜单，包含：
  - **Delete** — 所有非 running 状态可见
  - **Stop** — 仅 running 状态可见
  - **Re-run (Full)** — completed / failed 状态可见
  - **Re-run (Failed Only)** — 仅 failed 状态可见
- FR-05.2: 删除确认对话框：
  - 显示 Mission 标题
  - 勾选框"同时清理工作区目录"（默认勾选）
  - 警告文案"此操作不可撤销"
- FR-05.3: 重新执行确认对话框：
  - 显示模式说明（全部重跑 / 仅失败重跑）
  - 显示将被重置的任务数量
  - 确认后跳转到 StartMissionDialog 选择工作区

### 非功能需求

- NFR-01: 删除操作应在 500ms 内完成（不含磁盘清理）
- NFR-02: 磁盘清理在后台异步执行，不阻塞 UI
- NFR-03: 重新执行的重置操作应使用数据库事务，保证原子性

---

## AR — Architecture Requirements（架构需求）

### AR-01: 后端命令

| 命令 | 参数 | 返回 | 说明 |
|------|------|------|------|
| `delete_mission` | `mission_id: String, clean_workspace: bool` | `()` | 扩展现有命令，新增 `clean_workspace` 参数 |
| `stop_mission_execution` | `mission_id: String` | `()` | 停止调度器 + 重置 running tasks |
| `restart_mission` | `mission_id: String, mode: "full" \| "failed_only"` | `RestartResult { reset_count: u32 }` | 重置任务 + 清理 Agent 数据 |

### AR-02: 数据模型变更

需要记录 Mission 最近一次执行的工作区路径，用于清理：

```sql
-- 方案 A: 在 missions 表新增字段（推荐，简单）
ALTER TABLE missions ADD COLUMN repo_path TEXT;

-- start_mission_execution 时写入
UPDATE missions SET repo_path = ?1 WHERE id = ?2;
```

### AR-03: 数据库查询（queries.rs 新增）

```rust
/// 删除 mission 关联的所有 agents（及其 CASCADE 的 events、costs）
fn delete_agents_for_mission(conn: &Connection, mission_id: &str) -> Result<u64>;

/// 重置 mission 所有任务到初始态
fn reset_all_tasks(conn: &Connection, mission_id: &str) -> Result<u32>;

/// 仅重置失败任务
fn reset_failed_tasks(conn: &Connection, mission_id: &str) -> Result<u32>;
```

### AR-04: 前端接口（ipc/commands.ts 新增）

```typescript
interface DeleteMissionRequest {
  mission_id: string;
  clean_workspace: boolean;
}

interface RestartMissionRequest {
  mission_id: string;
  mode: "full" | "failed_only";
}

interface RestartResult {
  reset_count: number;
}
```

### AR-05: 事件

| 事件 | payload | 触发时机 |
|------|---------|---------|
| `mission-status-changed` | 复用现有 | stop / restart 改变状态时 |

无需新增事件，复用现有的 `mission-status-changed`。

### AR-06: 时序图

#### 删除流程

```
用户 → MissionList 右键 "Delete"
     → DeleteConfirmDialog (勾选清理工作区)
     → commands.deleteMission({ mission_id, clean_workspace: true })
     → [后端] 校验状态 ≠ running
     → [后端] DELETE FROM missions (CASCADE 清理子表)
     → [后端] 异步清理 worktree 目录
     → [前端] removeMission(id)
```

#### 重新执行流程

```
用户 → MissionList 右键 "Re-run (Full)"
     → RestartConfirmDialog (显示将重置 N 个任务)
     → commands.restartMission({ mission_id, mode: "full" })
     → [后端] 删除关联 agents + events + costs
     → [后端] 重置任务状态
     → [后端] Mission → planned
     → [前端] 刷新 Mission 详情
     → [前端] 打开 StartMissionDialog
     → 用户选择工作区 → startMissionExecution
```

---

## 与现有代码的关系

| 现有文件 | 需修改内容 |
|---------|-----------|
| `commands/mission.rs` · `delete_mission` | 放开状态限制，新增 `clean_workspace` 参数 |
| `commands/agent.rs` | 新增 `stop_mission_execution`、`restart_mission` |
| `db/queries.rs` | 新增重置和清理查询 |
| `db/migrations.rs` | 新增 migration: `missions.repo_path` 字段 |
| `agent/scheduler.rs` · `start_mission_execution` | 写入 `repo_path` 到 missions 表 |
| `src/ipc/commands.ts` | 新增接口和命令 |
| `src/views/MissionsView.tsx` | Mission 操作菜单 + 确认对话框 |
| `src/components/mission/MissionListItem.tsx` | 操作按钮/菜单 |
