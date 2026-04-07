# FM-08: Mission Lifecycle — 测试用例

> 版本: v1.0 | 日期: 2026-04-02  
> 对应需求: FM-08 requirements.md

---

## UT — 单元测试

### UT-01: 扩展删除（FR-01）


| ID      | 场景                   | 输入                                        | 预期结果                                   |
| ------- | -------------------- | ----------------------------------------- | -------------------------------------- |
| UT-01.1 | 删除 draft mission     | `delete_mission("m1", false)`             | 成功，missions/tasks 行被删除                 |
| UT-01.2 | 删除 planned mission   | `delete_mission("m1", false)`             | 成功                                     |
| UT-01.3 | 删除 completed mission | `delete_mission("m1", false)`             | 成功，关联 agents/events/costs 被 CASCADE 删除 |
| UT-01.4 | 删除 failed mission    | `delete_mission("m1", false)`             | 成功                                     |
| UT-01.5 | 删除 running mission   | `delete_mission("m1", false)`             | 失败，返回"请先停止执行"                          |
| UT-01.6 | 删除后验证 CASCADE        | 创建 mission + tasks + agents + events → 删除 | agents/events/costs 表零残留               |


### UT-02: 停止执行（FR-02）


| ID      | 场景                  | 输入                                       | 预期结果                                    |
| ------- | ------------------- | ---------------------------------------- | --------------------------------------- |
| UT-02.1 | 停止 running mission  | `stop_mission_execution("m1")`           | mission → failed, running tasks → ready |
| UT-02.2 | 停止非 running mission | `stop_mission_execution("m1")` (planned) | 返回错误                                    |


### UT-03: 全部重跑（FR-03）


| ID      | 场景                        | 输入                                     | 预期结果                                          |
| ------- | ------------------------- | -------------------------------------- | --------------------------------------------- |
| UT-03.1 | 重跑 completed mission（无依赖） | 3 tasks (all completed) → restart full | 3 tasks 变 ready, agents 被删, mission → planned |
| UT-03.2 | 重跑 completed mission（有依赖） | A→B→C (all completed) → restart full   | A → ready, B/C → pending                      |
| UT-03.3 | 重跑 failed mission         | 2 completed + 1 failed → restart full  | 3 tasks 全部重置                                  |
| UT-03.4 | 重跑 running mission        | → restart full                         | 返回错误                                          |
| UT-03.5 | 重跑 draft mission          | → restart full                         | 返回错误                                          |


### UT-04: 仅重跑失败（FR-04）


| ID      | 场景                              | 输入                  | 预期结果                                 |
| ------- | ------------------------------- | ------------------- | ------------------------------------ |
| UT-04.1 | 1 failed + 2 completed          | restart failed_only | 仅 1 task 重置为 ready, 2 completed 不变   |
| UT-04.2 | failed task 有上游依赖（上游 completed） | restart failed_only | failed → ready                       |
| UT-04.3 | failed task 有下游依赖（下游也 failed）   | restart failed_only | 两个都重置：上游 → ready, 下游 → pending       |
| UT-04.4 | 全部 completed 无失败                | restart failed_only | 返回提示"无需重跑"                           |
| UT-04.5 | 验证 completed task 的 agent 数据保留  | restart failed_only | completed tasks 的 agents/events 不受影响 |


### UT-05: DAG topo query — 复用测试


| ID      | 场景                           | 预期结果                                                     |
| ------- | ---------------------------- | -------------------------------------------------------- |
| UT-05.1 | `reset_all_tasks` 基本功能       | 所有 tasks 重置，assigned_agent_id 清空                         |
| UT-05.2 | `reset_failed_tasks` 重置后依赖正确 | failed task 有 completed 上游 → ready；有 failed 上游 → pending |


---

## IT — 集成测试

### IT-01: 删除端到端


| ID      | 场景                     | 步骤                         | 预期                            |
| ------- | ---------------------- | -------------------------- | ----------------------------- |
| IT-01.1 | 前端删除 completed mission | 右键 → Delete → 确认 → 勾选清理工作区 | Mission 从列表消失，磁盘 worktree 被清理 |
| IT-01.2 | 删除后 MissionDetail 正确处理 | 删除当前选中的 mission            | 自动选中列表中下一个 mission            |


### IT-02: 重新执行端到端


| ID      | 场景    | 步骤                           | 预期                      |
| ------- | ----- | ---------------------------- | ----------------------- |
| IT-02.1 | 全部重跑  | Re-run Full → 选工作区 → Start   | 所有任务重新执行，新的 agents 产生   |
| IT-02.2 | 仅失败重跑 | Re-run Failed → 选工作区 → Start | 仅失败任务被调度，completed 任务保持 |


### IT-03: 停止 + 重跑组合


| ID      | 场景       | 步骤                                 | 预期             |
| ------- | -------- | ---------------------------------- | -------------- |
| IT-03.1 | 运行中停止后重跑 | Start → Stop → Re-run Full → Start | Mission 成功重新执行 |


---

## BT — 边界测试


| ID    | 场景                               | 预期                          |
| ----- | -------------------------------- | --------------------------- |
| BT-01 | 删除一个有 100 个任务的 mission           | 500ms 内完成                   |
| BT-02 | 重跑时工作区目录已被手动删除                   | 重新创建 + git init，不报错         |
| BT-03 | 连续快速点击 Re-run 两次                 | 第二次返回错误（mission 已在 running） |
| BT-04 | 删除 mission 的同时 agent 正在写入 events | CASCADE 删除正常完成，不死锁          |


