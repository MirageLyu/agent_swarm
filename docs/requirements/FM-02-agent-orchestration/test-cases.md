# FM-02: Multi-Agent Orchestration — 测试用例

> 版本: v1.0 | 日期: 2026-04-01

---

## 单元测试 (UT)

### UT-01: 调度器选取 ready task（Rust）


| ID      | 场景            | 输入                  | 期望结果          |
| ------- | ------------- | ------------------- | ------------- |
| UT-01.1 | 单个 ready task | 1 ready / 0 running | 正确分配 1 个 task |
| UT-01.2 | 多个 ready task | 3 ready / 并发上限 2    | 仅启动 2 个 task  |
| UT-01.3 | 无 ready task  | 0 ready             | 不启动 Agent     |
| UT-01.4 | 原子抢占          | 两个调度循环同时尝试分配同一 task | 仅一个成功         |


### UT-02: 依赖推进（Rust）


| ID      | 场景      | 输入                  | 期望结果          |
| ------- | ------- | ------------------- | ------------- |
| UT-02.1 | 单依赖满足   | T2 依赖 T1，T1 完成      | T2 变为 ready   |
| UT-02.2 | 多依赖部分满足 | T3 依赖 T1,T2，仅 T1 完成 | T3 保持 pending |
| UT-02.3 | 多依赖全部满足 | T3 依赖 T1,T2，二者均完成   | T3 变为 ready   |
| UT-02.4 | 上游失败    | T2 依赖 T1，T1 failed  | T2 不变为 ready  |


### UT-03: Worktree 生命周期（Rust）


| ID      | 场景          | 操作                           | 期望结果      |
| ------- | ----------- | ---------------------------- | --------- |
| UT-03.1 | 创建 worktree | 调用 create_worktree(agent_id) | 目录和分支创建成功 |
| UT-03.2 | 重复 agent_id | 两次创建同一 agent_id              | 第二次返回错误   |
| UT-03.3 | 非 git 仓库    | 在非 repo 路径调用                 | 返回明确错误    |
| UT-03.4 | 保留 worktree | Agent 完成后不清理                 | 目录仍存在     |


### UT-04: Mission 终态判定（Rust）


| ID      | 场景       | 输入                                 | 期望结果                           |
| ------- | -------- | ---------------------------------- | ------------------------------ |
| UT-04.1 | 全部完成     | 全部 tasks completed                 | mission completed              |
| UT-04.2 | 存在失败无运行中 | 1 completed + 1 failed + 0 running | mission failed（已完成的 task 成果保留） |
| UT-04.3 | 存在运行中    | 1 running + 1 pending              | mission 保持 running（不触发终态）      |
| UT-04.4 | 全部取消     | 全部 tasks cancelled                 | mission failed                 |


---

## 集成测试 (IT)

### IT-01: 2 Agent 并行执行


| 步骤  | 操作                                     | 期望结果                             |
| --- | -------------------------------------- | -------------------------------- |
| 1   | 创建一个包含 3 个 task 的 Mission，其中 T1/T2 无依赖 | Mission 创建成功                     |
| 2   | 点击 `Confirm & Start`                   | Mission 进入 running               |
| 3   | 查看 Workspace                           | 同时出现 2 个 running Agent           |
| 4   | 检查文件系统                                 | 生成 2 个独立 `.worktrees/<agent_id>` |


### IT-02: 下游自动推进


| 步骤  | 操作                    | 期望结果               |
| --- | --------------------- | ------------------ |
| 1   | Mission 中 T3 依赖 T1,T2 | 初始 T3 为 pending    |
| 2   | 等待 T1 完成              | T3 仍为 pending      |
| 3   | 等待 T2 完成              | T3 自动变为 ready 并被调度 |


### IT-03: Agent 失败隔离


| 步骤  | 操作              | 期望结果                                     |
| --- | --------------- | ---------------------------------------- |
| 1   | 让其中一个 task 故意失败 | 对应 Agent 状态为 failed                      |
| 2   | 观察其他 Agent      | 其他 Agent 继续运行                            |
| 3   | 查看 Mission      | Mission 最终为 failed 或 partial failed 策略结果 |


---

## 边界测试 (BT)


| ID    | 场景            | 操作                          | 期望结果                 |
| ----- | ------------- | --------------------------- | -------------------- |
| BT-01 | 并发上限为 1       | 启动含 5 ready tasks 的 Mission | 串行执行，不超限             |
| BT-02 | worktree 创建失败 | 模拟磁盘权限异常                    | task 回退为 failed，错误可见 |
| BT-03 | 调度器重启恢复       | 运行中重启应用                     | 从数据库恢复未完成任务          |
| BT-04 | 主仓库有未提交变更     | 启动 Mission                  | 明确提示策略或允许继续但不污染主工作区  |
| BT-05 | 大量任务          | 50 tasks 混合依赖               | 调度延迟在可接受范围内          |


