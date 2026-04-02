# FM-01: Mission Planning & Task DAG — 测试用例

> 版本: v1.0 | 日期: 2026-04-01

---

## 单元测试 (UT)

### UT-01: Planner JSON 解析与校验（Rust）

| ID | 场景 | 输入 | 期望结果 |
|----|------|------|---------|
| UT-01.1 | 合法 JSON 解析 | 标准的 3 任务 JSON（无依赖） | 解析成功，返回 3 个 Task 结构体 |
| UT-01.2 | 合法依赖关系 | T1 无依赖，T2 depends_on [T1]，T3 depends_on [T1, T2] | 解析成功，依赖关系正确 |
| UT-01.3 | 循环依赖检测 | T1 depends_on [T2]，T2 depends_on [T1] | 返回 `CyclicDependency` 错误 |
| UT-01.4 | 引用不存在的 Task | T2 depends_on [T99] | 返回 `InvalidDependencyRef` 错误 |
| UT-01.5 | 自依赖检测 | T1 depends_on [T1] | 返回 `SelfDependency` 错误 |
| UT-01.6 | 空任务列表 | `{ "tasks": [] }` | 返回 `EmptyTaskList` 错误 |
| UT-01.7 | 非法 JSON | `"这不是JSON"` | 返回 `JsonParseError` |
| UT-01.8 | 缺失必要字段 | Task 缺少 `title` | 返回 `MissingField` 错误 |
| UT-01.9 | 非法 complexity 值 | `"complexity": "extreme"` | 返回 `InvalidComplexity` 错误 |
| UT-01.10 | 大规模 DAG | 30 个有复杂依赖的 tasks | 解析成功，无循环依赖 |

### UT-02: DAG 布局算法（TypeScript）

| ID | 场景 | 输入 | 期望结果 |
|----|------|------|---------|
| UT-02.1 | 线性依赖 | T1→T2→T3 | 3 层，每层 1 个节点 |
| UT-02.2 | 并行任务 | T1, T2 无依赖；T3 depends_on [T1, T2] | 2 层：[T1,T2] 和 [T3] |
| UT-02.3 | 钻石依赖 | T1→T2, T1→T3, T2→T4, T3→T4 | 3 层：[T1], [T2,T3], [T4] |
| UT-02.4 | 单节点 | 仅 T1 | 1 层 1 节点，坐标在可视区域中心 |
| UT-02.5 | 宽并行 | 10 个无依赖 task | 1 层 10 个节点，y 坐标均匀分布 |
| UT-02.6 | 深串行 | T1→T2→...→T10 | 10 层，每层 1 个节点 |
| UT-02.7 | 跨层依赖 | T1→T3（跳过 T2 所在层） | T3 在第 2 层（取最大依赖深度+1） |

### UT-03: Mission 数据库 CRUD（Rust）

| ID | 场景 | 操作 | 期望结果 |
|----|------|------|---------|
| UT-03.1 | 创建 Mission + Tasks | 插入 1 mission + 3 tasks + 2 dependencies | 所有记录可查询到 |
| UT-03.2 | 删除 Task 级联 | 删除 T2，T3 depends_on [T2] | T2 删除，task_dependencies 中 T3→T2 记录也删除 |
| UT-03.3 | 更新 Task 字段 | 更新 T1 的 title 和 description | 查询到新值，updated_at 更新 |
| UT-03.4 | Confirm Mission | 调用 confirm，Mission 有 3 tasks（T1 无依赖，T2 depends T1） | Mission.status=planned，T1.status=ready，T2.status=pending |
| UT-03.5 | 删除未启动 Mission | 删除 draft 状态的 Mission | Mission 和所有关联 tasks、dependencies 删除 |
| UT-03.6 | 不能删除运行中 Mission | 删除 running 状态的 Mission | 返回错误 |

### UT-04: PlanInput 组件（React）

| ID | 场景 | 操作 | 期望结果 |
|----|------|------|---------|
| UT-04.1 | 空输入不能提交 | 不输入文字，点击 Plan 按钮 | 按钮 disabled |
| UT-04.2 | 输入后可提交 | 输入"实现用户认证" | 按钮 enabled |
| UT-04.3 | 字符数限制 | 输入超过 2000 字符 | 输入被截断或显示字数提示 |
| UT-04.4 | Loading 状态 | 点击 Plan 后 | 按钮显示"Planning..."，输入框 disabled |
| UT-04.5 | Cmd+Enter 提交 | 输入文字后按 Cmd+Enter | 触发提交（同点击 Plan 按钮） |

---

## 集成测试 (IT)

### IT-01: 端到端 Mission Planning 流程

**前置条件**: 应用已启动，LLM API 可用

| 步骤 | 操作 | 期望结果 |
|:---:|------|---------|
| 1 | 导航到 Missions 视图 | 显示空状态提示"No missions yet" |
| 2 | 在输入框输入"为博客系统实现用户注册和登录功能" | 文字正常显示，Plan 按钮可用 |
| 3 | 点击 Plan Mission | 按钮变为 loading 状态 |
| 4 | 等待 LLM 响应（≤15s） | DAG 渲染出来，显示 3-8 个 task 节点 |
| 5 | 验证节点内容 | 每个节点有标题和 complexity 标签 |
| 6 | 验证依赖线 | 节点之间有合理的依赖连线 |
| 7 | 左侧 Mission 列表更新 | 新 Mission 出现在列表顶部，状态 badge 为"Draft" |

### IT-02: DAG 编辑流程

**前置条件**: IT-01 完成，DAG 已渲染

| 步骤 | 操作 | 期望结果 |
|:---:|------|---------|
| 1 | 点击某个 Task 节点 | 弹出操作菜单 |
| 2 | 选择"编辑" | 弹出编辑对话框，预填当前标题和描述 |
| 3 | 修改标题为"Updated Title" | 输入正常 |
| 4 | 点击保存 | 对话框关闭，节点标题更新为"Updated Title" |
| 5 | 点击另一个 Task，选择"删除" | 节点消失，关联的依赖线也消失 |
| 6 | 点击"添加任务" | 弹出新建任务表单 |
| 7 | 填写标题、描述，选择依赖 | 确认后新节点出现在 DAG 中 |

### IT-03: Confirm & Start 流程

**前置条件**: IT-02 完成，DAG 编辑后

| 步骤 | 操作 | 期望结果 |
|:---:|------|---------|
| 1 | 点击"Confirm & Start" | 按钮进入 loading 状态 |
| 2 | 等待后端响应 | Mission 状态变为"Planned"，左侧列表 badge 更新 |
| 3 | 自动跳转 | UI 切换到 WorkspaceView |
| 4 | 检查数据库 | `missions.status = 'planned'`，无依赖的 tasks `status = 'ready'` |

### IT-04: Mission 列表交互

| 步骤 | 操作 | 期望结果 |
|:---:|------|---------|
| 1 | 创建 3 个 Mission | 列表按创建时间倒序显示 3 项 |
| 2 | 点击第 2 个 Mission | 右侧面板切换到对应 Mission 的 DAG |
| 3 | 对 draft 状态的 Mission 点击删除 | Mission 从列表消失 |
| 4 | 对 planned 状态的 Mission 点击删除 | 显示错误提示或无删除按钮 |

---

## 边界测试 (BT)

| ID | 场景 | 操作 | 期望结果 |
|----|------|------|---------|
| BT-01 | LLM 返回非 JSON | Planner LLM 返回纯文本 | 自动重试一次，两次均失败后显示友好错误 |
| BT-02 | LLM 超时 | 网络延迟导致 15s 超时 | 显示"Planning timed out, please retry" |
| BT-03 | LLM 返回空 tasks | JSON 合法但 tasks 为空数组 | 显示"Planner produced no tasks, please provide more details" |
| BT-04 | API Key 未配置 | 未设置 API key 时点击 Plan | 显示明确提示"Please configure your API key in Settings first" |
| BT-05 | 并发 Plan | 快速双击 Plan 按钮 | 仅触发一次请求 |
| BT-06 | 极长输入 | 输入 2000 字符的需求描述 | 正常处理，不截断或丢失 |
| BT-07 | 特殊字符输入 | 输入包含 `<script>`, SQL 注入等 | 不执行，作为纯文本传递给 LLM |
| BT-08 | 网络断开 | Plan 过程中网络断开 | 显示网络错误提示，不卡死 |
| BT-09 | 应用重启恢复 | Plan 完成后重启应用 | Mission 和 Tasks 从数据库恢复，DAG 正常渲染 |
| BT-10 | 删除唯一 Task | DAG 只有 1 个 task 时删除它 | 允许删除，显示空 DAG 状态，Confirm 按钮 disabled |
