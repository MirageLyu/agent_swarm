# FM-09: UI Polish — 测试用例

> 版本: v1.0 | 日期: 2026-04-07

---

## 单元测试 (UT)

### UT-01: TopBar 实时指标（TS）

| ID | 场景 | 输入 | 期望结果 |
|----|------|------|---------|
| UT-01.1 | 有活跃 Mission | scheduler status: 3 running / 5 total | 显示 "3/5" |
| UT-01.2 | 无活跃 Mission | scheduler active_count = 0 | 指标区域隐藏 |
| UT-01.3 | 成本阈值 — 正常 | cost $3.85 / budget $30 (12.8%) | 默认色 |
| UT-01.4 | 成本阈值 — 警告 | cost $16 / budget $30 (53%) | 橙色 |
| UT-01.5 | 成本阈值 — 危险 | cost $25 / budget $30 (83%) | 红色 |
| UT-01.6 | 运行时长格式化 | 持续 125 秒 | 显示 "2m 5s" |

### UT-02: 终端行着色（TS）

| ID | 场景 | 输入 kind | 期望 CSS 类 |
|----|------|----------|-----------|
| UT-02.1 | LLM 调用 | `llm_call` | `.line-llm-call`（蓝色） |
| UT-02.2 | 工具使用 | `tool_use` | `.line-tool-use`（青色） |
| UT-02.3 | 工具结果 | `tool_result` | `.line-tool-result`（灰色） |
| UT-02.4 | 错误 | `error` | `.line-error`（红色） |
| UT-02.5 | 检查点 | `checkpoint` | `.line-checkpoint`（暗灰色） |
| UT-02.6 | 状态变更 | `status_change` | `.line-status-change`（橙色） |
| UT-02.7 | 便签应用 | `note_applied` | `.line-note-applied`（黄底） |
| UT-02.8 | 消息 | `message` | `.line-message`（白色） |

### UT-03: DAG 边状态映射（TS）

| ID | 场景 | 上游 task status | 期望边渲染 |
|----|------|----------------|----------|
| UT-03.1 | 上游已完成 | `completed` | 绿色实线 (#34C759) |
| UT-03.2 | 上游运行中 | `running` | 蓝色 (#007AFF) + 流动动画 |
| UT-03.3 | 上游待定 | `pending` / `ready` | 灰色虚线 (#8E8E93) |
| UT-03.4 | 上游失败 | `failed` | 红色实线 (#FF3B30) |

### UT-04: DAG 节点拖拽逻辑（TS）

| ID | 场景 | 操作 | 期望结果 |
|----|------|------|---------|
| UT-04.1 | 拖拽开始 | mousedown on node | `isDragging` 状态为 true，记录偏移量 |
| UT-04.2 | 拖拽移动 | mousemove | 节点 x/y 实时更新，SVG 边重绘 |
| UT-04.3 | 拖拽结束 | mouseup | `isDragging` false，位置保留在 state |
| UT-04.4 | Auto Layout | 点击 Auto Layout 按钮 | 所有节点位置恢复为 `computeDagLayout` 计算值 |
| UT-04.5 | 缩放下拖拽 | 缩放 150% 后拖拽 | 位移量正确除以 scale factor |

### UT-05: Command Palette 过滤（TS）

| ID | 场景 | 输入 | 期望结果 |
|----|------|------|---------|
| UT-05.1 | 空搜索 | "" | 显示全部命令 |
| UT-05.2 | 模糊匹配 | "mis" | 匹配 "New Mission" |
| UT-05.3 | 无匹配 | "zzz" | 显示 "No results" |
| UT-05.4 | 大小写不敏感 | "THEME" | 匹配 "Toggle Theme" |

### UT-06: Review 过滤逻辑（TS）

| ID | 场景 | 过滤条件 | 输入 agents | 期望结果 |
|----|------|---------|-----------|---------|
| UT-06.1 | All | `all` | 3 agents (1 approved, 1 needs review, 1 null) | 显示 3 个 |
| UT-06.2 | Needs Review | `needs_review` | 同上 | 显示 2 个（null + needs_review） |
| UT-06.3 | Approved | `approved` | 同上 | 显示 1 个 |
| UT-06.4 | 计数 Badge | — | 同上 | All:3 / Needs Review:2 / Approved:1 |

### UT-07: DAG 汇总栏计算（TS）

| ID | 场景 | 输入 tasks | 期望文本 |
|----|------|----------|---------|
| UT-07.1 | 混合状态 | 2 completed, 2 running, 2 pending | "6 tasks: 2 completed · 2 running · 2 pending" |
| UT-07.2 | 全部完成 | 5 completed | "5 tasks: 5 completed" |
| UT-07.3 | 含失败 | 3 completed, 1 failed | "4 tasks: 3 completed · 1 failed" |

---

## 集成测试 (IT)

### IT-01: TopBar 实时指标端到端

| 步骤 | 操作 | 期望结果 |
|------|------|---------|
| 1 | 创建并启动一个 Mission（2 个 task） | TopBar 显示 "0/3" → "1/3"（Agent 被分配后） |
| 2 | 等待 Agent 开始运行 | 运行时长开始计时，成本从 $0.00 开始增长 |
| 3 | Mission 完成 | 指标区域仍显示最终数据，时长停止计时 |

### IT-02: Grid 模式终端输出

| 步骤 | 操作 | 期望结果 |
|------|------|---------|
| 1 | 启动含 3 个并发 Agent 的 Mission | WorkspaceView 默认或切换为 Grid 模式 |
| 2 | 观察 Grid 面板 | 3 个终端面板并排，每个显示彩色终端输出 |
| 3 | Agent 运行中 | 终端末尾有闪烁光标 |
| 4 | 切换到 List 模式 | 回到卡片样式展示 |
| 5 | 点击某 Agent 的 Focus 按钮 | 进入 Focus 模式，单 Agent 全屏终端 |

### IT-03: DAG 节点拖拽 + 详情面板

| 步骤 | 操作 | 期望结果 |
|------|------|---------|
| 1 | 打开一个含 5 个 task 的 Mission DAG | 节点自动布局，边按状态着色 |
| 2 | 拖拽一个节点到新位置 | 节点移动，连线实时跟随重绘 |
| 3 | 点击某节点 | 右侧面板展开，显示任务详细信息 |
| 4 | 点击 Auto Layout | 所有节点动画回到自动计算位置 |
| 5 | 点击画布空白处 | 右侧面板折叠回空状态 |

### IT-04: Sidebar Agent 列表联动

| 步骤 | 操作 | 期望结果 |
|------|------|---------|
| 1 | 启动 Mission | Sidebar 底部 Agents 区域出现 Agent 列表 |
| 2 | 观察 Agent 运行 | 状态圆点为绿色脉冲，显示当前任务名 |
| 3 | 点击 Sidebar 中某 Agent | 切换到 Workspace → Focus 模式，聚焦该 Agent |
| 4 | Agent 完成 | 状态圆点变为绿色静态 |

### IT-05: Command Palette 操作

| 步骤 | 操作 | 期望结果 |
|------|------|---------|
| 1 | 按 ⌘K | Command Palette 浮层打开（带动画） |
| 2 | 输入 "theme" | 列表过滤为 "Toggle Theme" |
| 3 | 按 Enter 选中 | 主题切换，面板关闭 |
| 4 | 按 ⌘K 再按 Escape | 面板关闭 |

### IT-06: Code Review 过滤与批量操作

| 步骤 | 操作 | 期望结果 |
|------|------|---------|
| 1 | 打开一个已完成 Mission 的 Review 页面 | 显示所有 Agent 的 diff，汇总栏显示计数 |
| 2 | 点击 "Needs Review" 标签 | 只显示未审查的 Agent |
| 3 | 点击 "Approve All" | 所有 Agent 标记为 approved，Badge 更新 |
| 4 | 观察标签计数变化 | Needs Review 变为 0，Approved 增加 |

### IT-07: Settings 可编辑

| 步骤 | 操作 | 期望结果 |
|------|------|---------|
| 1 | 打开 Settings 页面 | 所有字段显示当前值，均可编辑 |
| 2 | 修改 Max Concurrent Agents 为 5 | 输入框值变为 5 |
| 3 | 点击 Save | 成功提示出现，重新 get_config 验证值已保存 |
| 4 | 修改 Base URL 为空 → Save | 保存成功（空值合法，表示恢复默认） |

---

## 边界测试 (BT)

| ID | 场景 | 操作 | 期望结果 |
|----|------|------|---------|
| BT-01 | Grid 模式 1 个 Agent | 只有 1 个 Agent 运行 | 单面板自适应全宽 |
| BT-02 | Grid 模式 6 个 Agent | 6 个 Agent 运行 | 2 列 × 3 行，可滚动 |
| BT-03 | 终端输出超长行 | 单行 > 500 字符 | 自动换行，不水平溢出 |
| BT-04 | 终端输出超多行 | > 1000 行 | 旧行自动裁剪，保持流畅 |
| BT-05 | DAG 节点拖出画布 | 拖到负坐标区域 | 节点位置 clamp 到画布边界 |
| BT-06 | DAG 30 个节点 | 大 DAG | 自动布局不重叠，缩放后可见全部 |
| BT-07 | Command Palette 快速开关 | 连续按 ⌘K 多次 | 不闪烁、不异常叠加 |
| BT-08 | Sidebar Agent 列表为空 | 无活跃 Mission | Agents 分组隐藏或显示 "No agents" |
| BT-09 | Approve All 无 Agent | Review 页无数据 | 按钮禁用 |
| BT-10 | Settings 并发保存 | 快速连续点击 Save | 不产生竞态，最后一次生效 |
