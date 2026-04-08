# FM-10: Pre-flight & Mission Contract — 测试用例

> 版本: v1.0 | 日期: 2026-04-08

---

## 单元测试 (UT)

### UT-01: Pre-flight 对话 Session 管理（Rust）

| ID | 场景 | 输入 | 期望结果 |
|----|------|------|---------|
| UT-01.1 | 创建 session | description + mode | session 和 mission 写入数据库，mission status = `preflight` |
| UT-01.2 | 追加消息 | session_id + user message | messages JSON 数组增长 |
| UT-01.3 | 切换模式 | session_id + new mode | session.mode 更新为新值 |
| UT-01.4 | 对话轮数上限 | 第 51 条消息 | 返回错误或自动压缩 |

### UT-02: Contract CRUD（Rust）

| ID | 场景 | 操作 | 期望结果 |
|----|------|------|---------|
| UT-02.1 | 添加条目 | add_contract_item(scope, "实现登录") | contract_items 增加 1 行，section=scope |
| UT-02.2 | 去重 | 添加两次相同文本 | 仅存储 1 条（或返回已存在提示） |
| UT-02.3 | 删除条目 | remove_contract_item(item_id) | 记录删除 |
| UT-02.4 | 更新配置 | update_contract_config(budget=50.0) | budget_usd=50.0 |
| UT-02.5 | 获取完整 Contract | get_contract(mission_id) | 返回四区块条目 + 配置 |
| UT-02.6 | 签署 | sign_contract(mission_id) | contract.status=signed，signed_at 有值 |
| UT-02.7 | 签署空 Contract | 无 scope 条目时签署 | 返回错误 |

### UT-03: Planner Agent — Contract 感知（Rust）

| ID | 场景 | 输入 | 期望结果 |
|----|------|------|---------|
| UT-03.1 | 含 Contract 的 system prompt | Contract 含 3 scope + 2 exclusions | system prompt 包含所有条目 |
| UT-03.2 | 生成 DAG 尊重 exclusions | exclusion="不实现 OAuth" | DAG 中无 OAuth 相关任务 |
| UT-03.3 | quality_threshold 影响 | threshold=9 | system prompt 强调高质量要求 |

### UT-04: PreflightChat 组件（TS）

| ID | 场景 | 操作 | 期望结果 |
|----|------|------|---------|
| UT-04.1 | 渲染 Agent 消息 | 收到 Agent 文本 | 灰底左对齐气泡 |
| UT-04.2 | 渲染 choice 按钮 | Agent 回复含 choices | 渲染可点击按钮组 |
| UT-04.3 | 选择 choice | 点击按钮 | 高亮选中、灰显其余、发送对应文本 |
| UT-04.4 | 自由输入 | 输入框输入 + Enter | 发送消息、输入框清空 |
| UT-04.5 | Typing indicator | Agent 回复前 | 显示三点跳动动画 |

### UT-05: ContractPanel 组件（TS）

| ID | 场景 | 输入 | 期望结果 |
|----|------|------|---------|
| UT-05.1 | 渲染四区块 | contract 数据 | 四个区块各显示正确计数和条目 |
| UT-05.2 | NEW 标签 | 新增条目 | "NEW" 标签显示 2 秒后淡出 |
| UT-05.3 | 删除条目 | 点击 × | 条目移除、计数更新 |
| UT-05.4 | 配置编辑 | 修改 budget | 值更新、自动保存 |

---

## 集成测试 (IT)

### IT-01: Pre-flight 完整流程

| 步骤 | 操作 | 期望结果 |
|------|------|---------|
| 1 | 输入需求"实现用户认证系统" → 点击 Pre-flight | 进入双栏界面，Agent 发出第一条场景走查消息 |
| 2 | Agent 提供选项"邮箱密码/OAuth/两者都要" | 渲染 3 个 choice 按钮 |
| 3 | 点击"邮箱密码" | 用户消息出现、Contract Scope 区增加 1 条 |
| 4 | 继续对话 3-5 轮 | Contract 各区块逐步填充 |
| 5 | 切换到"魔鬼代言人"模式 | Agent 开始质疑式提问 |
| 6 | 切换到"风险标记"模式 | Agent 识别安全风险 |
| 7 | 设置预算 $20、质量 8/10 | 配置卡数值更新 |
| 8 | 点击"签署合同并启动 Swarm" | 按钮 loading → DAG 生成 → 跳转 MissionsView |
| 9 | 查看 Mission 详情 | Contract 可查看（只读）、DAG 已生成 |

### IT-02: Quick Plan vs Pre-flight 入口

| 步骤 | 操作 | 期望结果 |
|------|------|---------|
| 1 | 输入需求 → 点击 "Quick Plan" | 沿用 FM-01 单轮 Planner，直接生成 DAG |
| 2 | 输入需求 → 点击 "Pre-flight" | 进入多轮对话界面 |

### IT-03: Contract 持久化与恢复

| 步骤 | 操作 | 期望结果 |
|------|------|---------|
| 1 | Pre-flight 对话 3 轮，Contract 有 2 条 scope | 数据已入库 |
| 2 | 关闭应用、重新打开 | 该 Mission 状态为 preflight |
| 3 | 点击该 Mission | 恢复对话界面，Contract 显示之前的 2 条 scope |

---

## 边界测试 (BT)

| ID | 场景 | 操作 | 期望结果 |
|----|------|------|---------|
| BT-01 | LLM 返回无 choices | Agent 回复纯文本 | 正常显示文本，无按钮 |
| BT-02 | 对话超长 | 50 轮对话 | 自动压缩早期消息或提示签署 |
| BT-03 | 签署空 Contract | 无 scope 条目时点签署 | 按钮 disabled 或报错 |
| BT-04 | 切换模式不丢数据 | 模式切换 | Contract 条目保留 |
| BT-05 | API Key 未配置 | 点击 Pre-flight | 提示配置 API Key |
| BT-06 | 网络中断 | 对话中网络断开 | 显示错误提示，对话可重发 |
| BT-07 | 并发签署 | 快速双击签署 | 仅触发一次 |
| BT-08 | 极长条目文本 | Contract 条目 > 500 字符 | 条目截断显示、完整存储 |
