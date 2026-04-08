# FM-14: Approval Queue — 测试用例

> 版本: v1.0 | 日期: 2026-04-08

---

## 单元测试 (UT)

### UT-01: 审批拦截逻辑（Rust）

| ID | 场景 | 输入 | 期望结果 |
|----|------|------|---------|
| UT-01.1 | 受保护路径匹配 | write_file → `package.json` | 触发审批 |
| UT-01.2 | 非受保护路径 | write_file → `src/main.ts` | 正常执行 |
| UT-01.3 | 破坏性命令匹配 | run_command → `rm -rf dist/` | 触发审批 |
| UT-01.4 | 安全命令 | run_command → `ls -la` | 正常执行 |
| UT-01.5 | 预算超 80% | 累计 cost=$25, budget=$30 | 触发审批 |
| UT-01.6 | 预算未超 80% | 累计 cost=$15, budget=$30 | 正常执行 |
| UT-01.7 | 自定义 protected path | 策略含 `*.env` | write_file → `.env` 触发审批 |
| UT-01.8 | 自定义 destructive command | 策略含 `docker rm` | run_command → `docker rm x` 触发审批 |

### UT-02: 审批请求 CRUD（Rust）

| ID | 场景 | 操作 | 期望结果 |
|----|------|------|---------|
| UT-02.1 | 创建请求 | create approval_request | status=pending，记录 agent_id/tool_name |
| UT-02.2 | Approve | resolve(approve) | status=approved，resolved_at 有值 |
| UT-02.3 | Reject | resolve(reject) | status=rejected |
| UT-02.4 | 查询 pending | list_pending_approvals | 仅返回 pending 状态 |
| UT-02.5 | 批量 approve | resolve_all(approve) | 所有 pending → approved |
| UT-02.6 | 重复操作 | 对已 approved 的请求再次 resolve | 返回错误或无操作 |

### UT-03: Agent 暂停/恢复（Rust）

| ID | 场景 | 操作 | 期望结果 |
|----|------|------|---------|
| UT-03.1 | 暂停 | 审批拦截 | Agent status → waiting_approval，执行循环暂停 |
| UT-03.2 | 恢复 | approve | Agent status → running，继续执行该 tool |
| UT-03.3 | 跳过 | reject | Agent 跳过该 tool，记录 skip 事件，继续下一步 |
| UT-03.4 | 超时过期 | 10 分钟无操作 | status → expired，Agent 跳过并继续 |

### UT-04: ApprovalQueue 组件（TS）

| ID | 场景 | 输入 | 期望结果 |
|----|------|------|---------|
| UT-04.1 | 渲染队列 | 3 个 pending 请求 | 3 张卡片横向排列 |
| UT-04.2 | 无请求 | 0 个 pending | 队列栏隐藏 |
| UT-04.3 | Approve 按钮 | 点击 Approve | 调用 resolve_approval，卡片淡出 |
| UT-04.4 | Reject 按钮 | 点击 Reject | 调用 resolve_approval，卡片淡出 |
| UT-04.5 | Approve All | 点击按钮 | 调用 resolve_all_approvals |
| UT-04.6 | 横向滚动 | 6 张卡片 | 可横向滚动浏览 |

### UT-05: ApprovalBadge 组件（TS）

| ID | 场景 | 输入 | 期望结果 |
|----|------|------|---------|
| UT-05.1 | 有审批 | pending_count=3 | 显示橙色 Badge "3" |
| UT-05.2 | 无审批 | pending_count=0 | Badge 隐藏 |
| UT-05.3 | 事件更新 | approval-requested 事件 | 计数 +1 |
| UT-05.4 | 事件更新 | approval-resolved 事件 | 计数 -1 |

### UT-06: 审批策略编辑器（TS）

| ID | 场景 | 操作 | 期望结果 |
|----|------|------|---------|
| UT-06.1 | 渲染策略 | 现有策略 | 显示 protected paths 和 destructive commands 列表 |
| UT-06.2 | 添加路径 | 输入 `*.secret` + 添加 | 列表增加一项 |
| UT-06.3 | 删除路径 | 点击 × | 该项移除 |
| UT-06.4 | 保存 | 点击 Save | 调用 update_approval_policy |

---

## 集成测试 (IT)

### IT-01: 审批完整流程 — Approve

| 步骤 | 操作 | 期望结果 |
|------|------|---------|
| 1 | 创建 Mission、配置 `package.json` 为受保护路径 | 策略生效 |
| 2 | 启动 Mission、Agent 执行 write_file → package.json | Agent 暂停 |
| 3 | 观察 UI | Approval Queue 栏出现、卡片显示操作详情、TopBar Badge "1" |
| 4 | 点击 Approve | 卡片消失、Queue 栏隐藏 |
| 5 | 观察 Agent | Agent 恢复执行、package.json 被修改 |

### IT-02: 审批完整流程 — Reject

| 步骤 | 操作 | 期望结果 |
|------|------|---------|
| 1 | Agent 请求执行 `rm -rf dist/` | 审批请求出现 |
| 2 | 点击 Reject | 卡片消失 |
| 3 | 观察 Agent | Agent 跳过该操作、Activity Stream 显示 skip 事件 |
| 4 | Agent 继续后续步骤 | 正常执行 |

### IT-03: 批量操作

| 步骤 | 操作 | 期望结果 |
|------|------|---------|
| 1 | 3 个 Agent 同时触发审批请求 | Queue 显示 3 张卡片 |
| 2 | 点击 Approve All | 所有卡片消失、3 个 Agent 恢复执行 |

### IT-04: 超时过期

| 步骤 | 操作 | 期望结果 |
|------|------|---------|
| 1 | Agent 触发审批请求 | 卡片出现 |
| 2 | 等待 10 分钟不操作 | 卡片自动消失、状态变为 expired |
| 3 | 观察 Agent | Agent 跳过该操作、继续执行 |

### IT-05: Badge 联动

| 步骤 | 操作 | 期望结果 |
|------|------|---------|
| 1 | 审批请求产生 | TopBar Badge "1"、Sidebar Agent 状态点变橙 |
| 2 | Approve | Badge 消失、状态点恢复绿色 |

### IT-06: 策略配置

| 步骤 | 操作 | 期望结果 |
|------|------|---------|
| 1 | Settings → Approval Policy | 显示当前策略 |
| 2 | 添加 `*.env` 到 Protected Paths → Save | 保存成功 |
| 3 | Agent 执行 write_file → .env.local | 触发审批（新规则生效） |

---

## 边界测试 (BT)

| ID | 场景 | 操作 | 期望结果 |
|----|------|------|---------|
| BT-01 | 空策略 | protected_paths=[], destructive_commands=[] | 所有操作直接执行、不拦截 |
| BT-02 | 大量待审批 | 20 个同时 pending | Queue 横向可滚动、TopBar Badge "20" |
| BT-03 | 快速审批 | 连续快速点击 Approve | 每次仅处理一个，不重复 |
| BT-04 | Agent 已取消 | Agent 被 kill 后仍有 pending 审批 | 审批卡片标记为 expired 或移除 |
| BT-05 | 审批中 Agent crash | Agent 异常退出 | 审批请求自动标记 expired |
| BT-06 | 预算边界 | cost 恰好 80% | 触发审批（含边界） |
| BT-07 | 通配符策略 | `*` 匹配所有路径 | 所有 write_file 触发审批 |
| BT-08 | 并发审批同一请求 | 两人同时 approve 同一请求 | 仅生效一次 |
