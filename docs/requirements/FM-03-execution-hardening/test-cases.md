# FM-03: Execution Engine Hardening — 测试用例

> 版本: v1.0 | 日期: 2026-04-01

---

## 单元测试 (UT)

### UT-01: 事件落库（Rust）

| ID | 场景 | 操作 | 期望结果 |
|----|------|------|---------|
| UT-01.1 | 写入 llm_call 事件 | 调用事件持久化函数 | `agent_events` 新增 1 行 |
| UT-01.2 | 写入 tool_result 事件 | 保存成功结果 | `kind=tool_result` 且 content 正确 |
| UT-01.3 | 写入 error 事件 | 保存错误 | 查询时可见错误详情 |

### UT-02: 工具错误结构化（Rust）

| ID | 场景 | 输入 | 期望结果 |
|----|------|------|---------|
| UT-02.1 | shell 成功 | exit code 0 | 返回 stdout，`is_error = false` |
| UT-02.2 | shell 失败 | exit code 1 | 返回 `{ error: "shell_error", message: "..." }`，`is_error = true` |
| UT-02.3 | shell 返回含 exit code | exit code 127（command not found） | 错误信息包含 exit code 和 stderr |
| UT-02.4 | read_file 不存在 | 不存在的路径 | 返回 `{ error: "file_not_found", message: "..." }`，`is_error = true` |
| UT-02.5 | 路径越界 | `../../etc/passwd` | 返回 `{ error: "sandbox_violation", message: "..." }`，`is_error = true` |
| UT-02.6 | write_file 成功 | 合法路径 + 内容 | 返回成功信息，`is_error = false` |

### UT-03: CancellationToken（Rust）

| ID | 场景 | 操作 | 期望结果 |
|----|------|------|---------|
| UT-03.1 | 正常取消 | 发送 stop_agent | checkpoint 处停止 |
| UT-03.2 | 重复取消 | 两次 stop_agent | 幂等成功 |
| UT-03.3 | 已完成 Agent | 对 completed agent stop | 返回无害结果，不报错 |

---

## 集成测试 (IT)

### IT-01: Agent 执行历史恢复

| 步骤 | 操作 | 期望结果 |
|:---:|------|---------|
| 1 | 启动一个 Agent 执行多步任务 | 产生多条事件 |
| 2 | 重启应用 | 应用正常启动 |
| 3 | 打开 Workspace | 可从数据库看到历史事件 |

### IT-02: 取消运行中 Agent

| 步骤 | 操作 | 期望结果 |
|:---:|------|---------|
| 1 | 启动长任务 Agent | 状态 running |
| 2 | 点击 Stop | 后端收到 stop_agent |
| 3 | 等待一个 checkpoint 周期 | Agent 状态变为 cancelled |
| 4 | 检查后续日志 | 不再有新的 llm/tool 事件 |

### IT-03: 工具失败可见化

| 步骤 | 操作 | 期望结果 |
|:---:|------|---------|
| 1 | 让 Agent 执行一个失败的 shell 命令 | 命令失败 |
| 2 | 查看活动流 | 展示标准化错误与 exit code |
| 3 | 查看数据库 | 对应 error/checkpoint 事件已落库 |

---

## 边界测试 (BT)

| ID | 场景 | 操作 | 期望结果 |
|----|------|------|---------|
| BT-01 | 数据库瞬时写入失败 | 模拟 SQLite busy | 重试或记录错误，不崩溃 |
| BT-02 | 取消时正在等待 LLM 返回 | stop_agent | 本轮结束后停止，不再进入下一步 |
| BT-03 | 高频事件 | 单 Agent 100+ 事件 | 落库性能可接受 |
| BT-04 | 非法 agent_id | 查询/停止不存在的 Agent | 返回明确错误 |
