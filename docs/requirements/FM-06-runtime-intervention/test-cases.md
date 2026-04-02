# FM-06: Runtime Intervention — 测试用例

> 版本: v1.0 | 日期: 2026-04-01

---

## 单元测试 (UT)

### UT-01: Note 队列（Rust）

| ID | 场景 | 操作 | 期望结果 |
|----|------|------|---------|
| UT-01.1 | 新建 note | inject_agent_note | 状态为 queued |
| UT-01.2 | 消费 note | Agent checkpoint poll | 状态变为 applied |
| UT-01.3 | Agent 已结束 | inject to completed agent | 返回错误或 expired |

### UT-02: 上下文拼接（Rust）

| ID | 场景 | 输入 | 期望结果 |
|----|------|------|---------|
| UT-02.1 | 单条 note | 1 queued note | 拼接到下一轮请求 |
| UT-02.2 | 多条 note | 3 queued notes | 按时间顺序拼接 |

---

## 集成测试 (IT)

### IT-01: 运行中注入生效

| 步骤 | 操作 | 期望结果 |
|:---:|------|---------|
| 1 | 启动一个多步 Agent | Agent 处于 running |
| 2 | 在 Workspace 输入 note | UI 显示 queued |
| 3 | 等待下一个 checkpoint | note 状态变为 applied |
| 4 | 查看活动流 | 可见 note 已注入的事件 |

### IT-02: 结束前未消费

| 步骤 | 操作 | 期望结果 |
|:---:|------|---------|
| 1 | Agent 即将完成时发送 note | note queued |
| 2 | Agent 完成且未再进入下一步 | note 变为 expired |

---

## 边界测试 (BT)

| ID | 场景 | 操作 | 期望结果 |
|----|------|------|---------|
| BT-01 | 空 note | 提交空文本 | 前端禁用或后端拒绝 |
| BT-02 | 超长 note | 提交超长文本 | 截断或返回错误 |
| BT-03 | 高频注入 | 快速发送多条 note | 顺序保持正确 |
| BT-04 | Agent 被取消 | queued 后立即 stop_agent | note 最终 expired 或 cancelled |
