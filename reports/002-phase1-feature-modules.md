# Dev Report #002: Phase 1 Feature Modules (FM-01 ~ FM-08)

**日期**: 2026-04-07  
**范围**: Phase 1 全部功能模块（核心循环验证）  
**代码量**: 14,141 行（Rust 6,750 / TS+TSX 4,720 / CSS 2,671）  
**Rust 测试用例**: 65 个  
**前端测试文件**: 2 个（`PlanInput.test.tsx`、`dag-layout.test.ts`）  
**数据库迁移**: 8 个（001_initial → 008_mission_repo_path）  
**Git 提交**: 7 commits on `main`

---

## 第一部分：概要

Phase 1 的目标是验证产品的核心循环：**用户输入需求 → Planner 分解任务 → 多 Agent 并行执行 → 活动流实时展示 → 代码审查 → 运行时介入 → Mission 生命周期管理**。

在 Phase 0-2 建立的基础设施（3,009 行）之上，8 个功能模块的开发将代码量从 3,009 行增长至 14,141 行（+11,132 行，370% 增幅），覆盖了产品 Phase 1 清单中的全部 9 项"必须有"功能。

### 核心交付物

| 交付物 | 状态 |
|--------|:---:|
| Planner Agent：LLM 驱动的任务分解 + DAG 验证 | ✅ |
| 多 Agent 并行调度器 + Git Worktree 隔离 | ✅ |
| 步进式执行引擎 + Checkpoint 持久化 + CancellationToken 取消 | ✅ |
| 活动流 UI（多 Agent 卡片 + 时间线 + 流式文本） | ✅ |
| 成本追踪与汇总（Per-Agent / Per-Mission） | ✅ |
| Monaco Editor 集成 + 结构化 Diff 审查 | ✅ |
| 便签条注入（Agent 级 + Mission 级 + 持久化指令） | ✅ |
| DAG 可视化（拓扑布局 + 缩放/平移 + 节点编辑） | ✅ |
| Mission 生命周期（删除 + 停止 + 重新执行） | ✅ |
| Agent 分支自动合并到 main（DAG 拓扑序 + 冲突自动解决） | ✅ |

---

## 第二部分：详细串讲

### FM-01: Mission Planning & Task DAG

**职责**: 用户输入需求 → Planner Agent 调用 LLM 分解任务 → DAG 展示与编辑

**后端实现 (Rust)**:

| 文件 | 行数 | 职责 |
|------|:---:|------|
| `agent/planner.rs` | 500 | Planner Agent 核心：LLM 流式调用 + JSON 解析 + DAG 环检测 + 错误重试 |
| `commands/mission.rs` | 884 | Mission CRUD + `plan_mission` + `confirm_mission` + DAG 编辑命令 |

**核心设计**:

- **Planner 系统 Prompt**: 指示 LLM 输出结构化 JSON（`mission_title` + `tasks[]`），每个任务包含 `id`/`title`/`description`/`complexity`/`depends_on`
- **解析与验证**: `parse_and_validate()` 执行 5 层校验——JSON 合法性、必填字段、complexity 枚举、依赖引用有效性、环检测（DFS 三色标记）
- **流式输出**: 通过 `planner-stream` 事件实时推送 `reasoning_delta`（思考过程）和 `text_delta`（JSON 输出），支持 Qwen 的 `reasoning_content` 字段
- **JSON 提取**: 支持纯 JSON、```json 代码块、混合文本等多种 LLM 输出格式
- **错误自修复**: 首次解析失败时，将错误信息反馈给 LLM 进行一次自动重试
- **Mission 状态机**: `draft` → `planned`（confirm 后无依赖的 task 自动变为 `ready`）

**DAG 编辑命令**: `update_task`（标题/描述/状态）、`delete_task`（级联删除依赖）、`add_task`（含依赖建立）

**前端实现**:

| 文件 | 行数 | 职责 |
|------|:---:|------|
| `components/mission/PlanInput.tsx` | 159 | 需求输入面板，调用 `plan_mission` |
| `components/mission/PlannerStreamPanel.tsx` | 78 | Planner 实时输出展示（思考过程 + JSON 输出） |
| `components/mission/TaskDAG.tsx` | 102 | DAG 容器，协调节点和连线渲染 |
| `components/mission/dag-layout.ts` | 190 | 拓扑排序 + 分层布局算法 |
| `components/mission/TaskNode.tsx` | 109 | DAG 节点组件（状态着色、点击编辑） |
| `components/mission/TaskEdge.tsx` | 21 | DAG 连线组件（SVG path） |
| `components/mission/TaskEditDialog.tsx` | 197 | 任务编辑对话框（标题/描述/状态） |
| `components/mission/MissionListItem.tsx` | 104 | Mission 列表项（进度条 + 操作按钮） |
| `views/MissionsView.tsx` | 380 | 主视图：Mission 列表 + DAG + 操作栏 |

**测试覆盖**: Rust 12 个 UT（解析验证的正常/异常路径 + 数据库操作），前端 2 个测试文件（PlanInput + dag-layout）

---

### FM-02: Multi-Agent Orchestration

**职责**: Agent 调度器 + Git Worktree 隔离 + 并行执行 + 分支自动合并

**后端实现**:

| 文件 | 行数 | 职责 |
|------|:---:|------|
| `agent/scheduler.rs` | 452 | 调度器核心：Mission 轮询循环 + 任务分发 + 依赖推进 + 分支合并 |
| `git/worktree.rs` | 628 | Worktree 管理：创建/删除/提交/合并/Diff |
| `commands/agent.rs` | 696 | Agent 相关命令：`start_mission_execution` + `stop_agent` + 查询 |

**核心设计**:

- **Scheduler 架构**: 每个 Mission 一个独立的 `tokio::spawn` 异步循环，1 秒轮询间隔，Mission 级 `CancellationToken` 控制停止
- **任务分发流程**: `poll_and_dispatch()` → 查 `ready` 状态 task（受 `max_concurrent_agents` 槽位限制）→ `claim_task()`（原子 CAS：`ready → running`）→ `dispatch_task()`
- **Agent 生命周期**: 创建 worktree → 注册 AgentRegistry → 构造 AgentEngine → `tokio::spawn` 运行 → 完成后 commit + 推进依赖
- **依赖推进**: `advance_dependencies()` 在 task 完成后检查下游 task 的所有上游是否全部完成，满足则 `pending → ready`
- **Mission 终态判定**: `check_mission_terminal()` 检查——全部 completed → mission completed；有 failed 且无 running/ready → mission failed
- **分支合并**: Mission 全部完成后，`merge_completed_mission()` 按 DAG 拓扑序（`get_completed_agents_topo_order()`）逐个合并 Agent 分支到 main
- **冲突自动解决**: 遵循 `git-operations.mdc` 规则——冲突文件接受 Agent 分支版本（theirs），因后合并的 Agent 在 DAG 下游、改动优先级更高
- **Mission 指令**: Scheduler 在分发任务时注入 `missions.directives`，确保后续 Agent 遵循全局约束
- **Base/Head Commit Hash**: 记录每个 Agent 的起始和结束 commit hash，支持分支删除后仍可查看 diff

**事件推送**: `agent-started`、`task-status-changed`、`mission-status-changed`、`mission-merge-progress`、`mission-merge-completed`

**测试覆盖**: Rust 12 个 UT（任务选择/并发限制/原子 claim/依赖推进/终态判定），Git 合并 6 个 UT（fast-forward/无冲突/单文件冲突/多文件冲突/空 commit/结构化 diff）

---

### FM-03: Execution Engine Hardening

**职责**: Checkpoint 持久化 + Agent 取消 + Schema 演化

**后端实现**:

| 文件 | 行数 | 关键变更 |
|------|:---:|------|
| `agent/engine.rs` | 445 | 每步骤 `persist_event()` 写入 `agent_events` + `persist_cost_record()` + `CancellationToken` 三次检查 |
| `agent/registry.rs` | 75 | `AgentRegistry`：全局 `HashMap<String, CancellationToken>` + `register()`/`cancel()`/`remove()` |
| `db/queries.rs` | 1302 | 所有数据库操作的集中查询模块 |
| `db/migrations.rs` | 197 | 8 个迁移脚本 |

**核心设计**:

- **Checkpoint 持久化**: `emit_event()` 同时做两件事——(1) Tauri `emit()` 推送前端，(2) `persist_event()` 写入 SQLite `agent_events` 表。每个步骤记录 `llm_call`→`tool_use`→`tool_result`→`checkpoint` 完整链
- **三重取消检查**: 循环顶部（进入前检查）→ LLM 响应后（处理前检查）→ 工具执行后（下一步前检查），任意节点取消立即 `finish_cancelled()` → 清理 notes → 状态更新为 `cancelled`
- **AgentRegistry**: 线程安全的 Token 注册表，支持 `stop_agent` 命令精准取消单个 Agent
- **成本记录**: 每步骤 `persist_cost_record()` 写入 `cost_records` 表 + `accumulate_agent_cost()` 更新 `agents` 表累计值
- **Schema 演化**: 7 个增量迁移

| 迁移 | 内容 |
|------|------|
| 001_initial | 6 张核心表 |
| 002_engine_hardening | `agent_events` 增加 `step` 列 + `status_change` kind |
| 003_review_event_kind | 增加 `review` kind（FM-05） |
| 004_agent_commit_hashes | `agents` 增加 `base_commit_hash` / `head_commit_hash` |
| 005_agent_notes | 创建 `agent_notes` 表（FM-06） |
| 006_agent_notes_mission_scope | 增加 `mission_id` 列 |
| 007_mission_directives | `missions` 增加 `directives` 列 |
| 008_mission_repo_path | `missions` 增加 `repo_path` 列（FM-08） |

**测试覆盖**: AgentRegistry 4 个 UT + 事件持久化 6 个 UT

---

### FM-04: Activity Stream & Cost Tracking

**职责**: 多 Agent 活动流 UI 增强 + 实时成本追踪

**后端实现**:

| 功能 | 位置 | 说明 |
|------|------|------|
| Mission 成本汇总 | `queries::get_mission_cost_summary()` | 跨 Agent/Task 聚合 `cost_records` |
| 灵活事件查询 | `queries::list_agent_events()` | 支持按 `agent_id` / `mission_id` / 全局三种粒度 |
| 命令封装 | `commands/agent.rs` → `get_mission_cost_summary`、`list_agent_events` | IPC 命令 |

**前端实现**:

| 组件 | 行数 | 职责 |
|------|:---:|------|
| `AgentStreamList.tsx` | 37 | 多 Agent 卡片列表，概览模式 |
| `AgentStreamCard.tsx` | 112 | 单 Agent 事件卡片（状态 Badge + 最近事件摘要 + 点击进入详情） |
| `AgentTimeline.tsx` | 128 | 单 Agent 详细时间线（全部事件 + 流式文本） |
| `CostSummaryBar.tsx` | 49 | 成本汇总栏（总成本 + input/output tokens） |
| `WorkspaceView.tsx` | 365 | 工作区主视图：Mission 选择 + Agent 概览/详情切换 + 成本栏 + 介入面板 |

**UI 模式**: 列表概览（`AgentStreamList`）→ 点击进入单 Agent 时间线（`AgentTimeline`）→ 返回列表，而非浏览器式标签页

**测试覆盖**: Rust 5 个 UT（成本汇总单 Agent/多 Agent/空记录 + 事件查询按 agent/mission）

---

### FM-05: Code Review & Diff

**职责**: Monaco Editor 集成 + 结构化 Diff 审查 + Review 操作

**后端实现**:

| 文件 | 行数 | 职责 |
|------|:---:|------|
| `commands/review.rs` | 118 | `get_agent_diff` + `submit_review_action` |
| `git/worktree.rs`（扩展） | — | `get_structured_diff()` + `get_structured_diff_by_hashes()` + `diff_trees()` |
| `db/queries.rs`（扩展） | — | `get_latest_review_status()` + `get_agent_commit_hashes()` |

**核心设计**:

- **结构化 Diff**: `DiffFile { path, status, old_content, new_content }` — 不是 patch 格式，而是完整文件内容对，直接驱动 Monaco DiffEditor
- **双源 Diff**: 优先使用分支对比（`get_structured_diff`）；分支被删除后回退到 commit hash 对比（`get_structured_diff_by_hashes`），确保合并后仍可审查
- **Review Actions**: `approved` / `rejected` / `revision_requested`（需要 comment），以 `review` kind 事件存入 `agent_events`
- **校验**: `revision_requested` 必须附带非空 comment；action 必须在合法枚举内

**前端实现**:

| 组件 | 行数 | 职责 |
|------|:---:|------|
| `DiffViewer.tsx` | 78 | Monaco `DiffEditor` 封装，inline/side-by-side 模式 |
| `DiffFileTree.tsx` | 43 | 左侧文件树（路径 + 状态标记：A/M/D） |
| `AgentReviewTabs.tsx` | 52 | Agent 标签切换（多 Agent review） |
| `ReviewActionBar.tsx` | 90 | Review 操作栏：Approve / Reject / Request Revision |
| `ReviewView.tsx` | 203 | Review 主视图：文件树 + Diff + 操作栏 |

**依赖新增**: `monaco-editor ^0.55.1` + `@monaco-editor/react ^4.7.0`

**测试覆盖**: Rust 4 个 UT（结构化 Diff 单/多文件 + 空变更 + hash 回退） + 3 个 Review 状态 UT

---

### FM-06: Runtime Intervention

**职责**: 便签条注入（Agent 级 + Mission 级）+ 持久化指令

**后端实现**:

| 功能 | 位置 | 说明 |
|------|------|------|
| 便签条存储 | `agent_notes` 表（005+006 迁移） | `id`/`agent_id`/`mission_id`/`content`/`status`（queued→applied/expired） |
| 引擎消费 | `engine.rs` → `poll_queued_notes()` | 每步工具执行后轮询 queued notes，格式化为 `[System Note - Priority Update from Commander]` 注入对话上下文 |
| 生命周期管理 | `mark_notes_applied()` / `expire_notes_for_agent()` | Applied 记录时间戳，Agent 结束时 expire 未消费的 notes |
| Mission 级广播 | `inject_mission_note` | 查询 Mission 下所有 running Agent，为每个 Agent 创建一条 note |
| 持久化指令 | `missions.directives` 列 | Mission 级全局约束，Scheduler 分发任务时自动附加到 task description |
| 命令 | `inject_agent_note` / `inject_mission_note` / `list_agent_notes` / `list_mission_notes` | 4 个 IPC 命令 |

**前端实现**:

| 组件 | 行数 | 职责 |
|------|:---:|------|
| `InterventionPanel.tsx` | 127 | Agent 级便签条输入（选择目标 Agent + 内容 + 发送） |
| `MissionNoteBar.tsx` | 82 | Mission 级便签条输入 + 历史记录展示 |

**注入格式**:
```
[System Note - Priority Update from Commander]:
The following directive(s) have been issued by the human commander.
You MUST follow them and adjust your work accordingly...

<directive content>

Please take this into account in your next steps.
```

**测试覆盖**: Rust 10 个 UT（插入/应用/过期/排序/上限/隔离/Mission 广播/跳过已完成 Agent）

---

### FM-07: Planning UX Enhancements

**职责**: DAG 画布缩放/平移 + Planner 流式输出 UI

**前端实现**:

| 组件 | 行数 | 职责 |
|------|:---:|------|
| `DAGViewport.tsx` | 253 | 核心：`ViewportTransform`（scale/translateX/translateY）+ `handleWheel` 缩放（0.3x~3x）+ 拖拽平移 + `fit-to-view` |
| `PlannerStreamPanel.tsx` | 78 | 实时展示 Planner 的思考过程（`reasoning_delta`）和 JSON 输出（`text_delta`） |

**后端支持**:

- `planner.rs` 中 `emit_planner_event()` 推送三种事件：`reasoning_delta`（思考过程）、`text_delta`（JSON 输出）、`done`/`error`
- `events.ts` 中 `onPlannerStream()` 监听事件

---

### FM-08: Mission Lifecycle

**职责**: Mission 删除 + 停止运行中 Mission + 重新执行（全量/仅失败）

**后端实现**:

| 命令 | 行为 |
|------|------|
| `delete_mission` | 校验非 running → 删除关联 Agents → 删除 Mission（CASCADE 删 tasks/deps）→ 可选清理 worktrees |
| `stop_mission_execution` | 校验 running → `scheduler.stop_mission()` 取消 Token → reset orphaned running tasks → 状态 → failed |
| `restart_mission` | 校验 completed/failed → `full` 模式：删除所有 Agent + reset 全部 task → `failed_only` 模式：只重置失败/取消的 task → 推进依赖已满足的 task 为 ready |

**核心设计**:

- **安全防护**: running 状态的 Mission 不可删除，必须先 stop
- **全量重启**: 清除所有 Agent 和事件数据，回到 planned 起点重新执行
- **仅失败重启**: 保留已完成的 task 和 Agent 产出，只重做失败的部分
- **Worktree 清理**: 异步 `tokio::spawn` 清理 `.worktrees` 目录，不阻塞用户操作

**前端实现**:

| 组件 | 行数 | 职责 |
|------|:---:|------|
| `DeleteConfirmDialog.tsx` | 56 | 删除确认（可选清理工作区） |
| `RestartConfirmDialog.tsx` | 62 | 重启确认（选择全量/仅失败模式） |
| `StartMissionDialog.tsx` | 116 | 启动执行对话框（选择仓库路径） |
| `MissionListItem.tsx` | 104 | Mission 列表项（集成所有生命周期操作按钮） |

**测试覆盖**: Rust 2 个 UT（删除 cascade + running 不可删除）

---

## 第三部分：How Far Are We

### 对照产品 Phase 1 清单（9 项"必须有"）

| # | 功能 | Phase 0-2 | Phase 1 FM | 完成度 |
|---|------|:---:|:---:|:---:|
| 1 | 对话面板：输入需求，Planner 分解任务 | 20% | FM-01 | **100%** |
| 2 | 2-3 个 Agent 并行执行（worktree 隔离） | 30% | FM-02 | **100%** |
| 3 | 步进式执行引擎 + Harness 检查点 | 60% | FM-03 | **100%** |
| 4 | 基础 Schema 验证 | 0% | FM-03 | **100%** |
| 5 | Agent 活动实时流 | 70% | FM-04 | **100%** |
| 6 | 统一 Code Diff 审查 | 0% | FM-05 | **100%** |
| 7 | Per-Task 成本追踪 | 30% | FM-04 | **100%** |
| 8 | 便签条注入（运行时介入） | 0% | FM-06 | **100%** |
| 9 | 基础 DAG 编辑 | 0% | FM-01+FM-07 | **100%** |

### 额外完成项（超出原始 9 项清单）

| 功能 | 来源 |
|------|------|
| Planner 流式输出（reasoning + text）| FM-07 |
| DAG 画布缩放/平移/fit-to-view | FM-07 |
| Mission 删除 + 停止 + 重新执行 | FM-08 |
| Agent 分支自动合并（DAG 拓扑序 + 冲突自动解决）| FM-02 |
| Mission 级持久化指令（directives）| FM-06 |
| Hash-based Diff 回退（分支删除后仍可审查）| FM-05 |

### 进度矩阵

```
基础设施     ████████████████████ 100%  (Phase 0-2)
功能模块     ████████████████████ 100%  (FM-01 ~ FM-08, Phase 1 全部完成)
Rust 测试    ████████████████░░░░  80%  (65 个 UT，核心逻辑覆盖良好)
前端测试     ██░░░░░░░░░░░░░░░░░░  10%  (2 个测试文件，需补充)
集成测试     ░░░░░░░░░░░░░░░░░░░░   0%  (端到端测试待建立)
```

### 代码结构最终快照

```
src-tauri/src/               6,750 行 Rust
├── agent/                   1,482 行
│   ├── engine.rs              445  — 步进式执行循环 + Checkpoint + 取消
│   ├── scheduler.rs           452  — Mission 调度器 + 任务分发 + 合并
│   ├── planner.rs             500  — Planner Agent (LLM + 解析 + 验证)
│   ├── registry.rs             75  — CancellationToken 注册表
│   ├── types.rs                30  — AgentStatus/AgentStep 类型
│   └── mod.rs                  10
├── commands/                1,745 行
│   ├── mission.rs             884  — Mission CRUD + DAG 编辑 + 生命周期
│   ├── agent.rs               696  — Agent 运行 + 调度 + 查询
│   ├── config.rs              136  — 配置管理（API Key / Provider / Model）
│   ├── review.rs              118  — Code Review 命令
│   ├── system.rs               34  — 系统信息
│   └── mod.rs                  11
├── db/                      1,546 行
│   ├── queries.rs            1302  — 全部数据库查询（含 65 个 UT 中的大部分）
│   ├── migrations.rs          197  — 8 个 Schema 迁移
│   ├── pool.rs                 37  — SQLite 连接管理
│   └── mod.rs                  10
├── git/                       631 行
│   ├── worktree.rs            628  — 完整 Worktree 管理（创建/合并/Diff）
│   └── mod.rs                   3
├── llm/                       678 行
│   ├── openai_compat.rs       392  — OpenAI Compatible API（DashScope）
│   ├── anthropic.rs           180  — Anthropic Messages API
│   ├── types.rs                77  — 内部类型定义
│   ├── provider.rs             17  — LlmProvider trait
│   └── mod.rs                   9
├── tools/                     416 行
│   ├── executor.rs            348  — 工具沙箱执行器
│   ├── definitions.rs          63  — 5 个内置工具 JSON Schema
│   └── mod.rs                   5
├── lib.rs                      86  — Tauri Builder + 32 个命令注册
└── main.rs                      5

src/                         4,720 行 TS/TSX
├── components/
│   ├── mission/             1,332 行  — DAG 可视化 + Mission 列表 + 对话框
│   ├── workspace/             535 行  — 活动流 + 成本栏 + 介入面板
│   ├── review/                363 行  — Monaco Diff + Review 操作
│   ├── ui/                    163 行  — Button / Input / Badge
│   ├── Sidebar.tsx            127
│   └── Titlebar.tsx            83
├── views/
│   ├── MissionsView.tsx       380  — Mission 管理主视图
│   ├── WorkspaceView.tsx      365  — 工作区主视图
│   ├── ReviewView.tsx         203  — Code Review 主视图
│   ├── SettingsView.tsx        94  — 设置视图
│   └── AgentsView/InsightsView 20  — 占位视图
├── ipc/                       473 行  — Tauri 命令/事件 TS 封装
├── stores/                    291 行  — Zustand 全局状态（ui/agent/task）
├── hooks/                      19  — useTheme
└── styles/                     — (CSS 变量 + 全局样式)

CSS                          2,671 行
```

### Tauri 命令注册总览（32 个）

| 模块 | 命令 | 数量 |
|------|------|:---:|
| System | `get_app_info`, `get_db_status` | 2 |
| Config | `get_config`, `set_api_key`, `update_config` | 3 |
| Mission | `create_mission`, `list_missions`, `plan_mission`, `get_mission_detail`, `confirm_mission`, `delete_mission`, `stop_mission_execution`, `restart_mission` | 8 |
| Task | `update_task`, `delete_task`, `add_task` | 3 |
| Agent | `run_agent`, `stop_agent`, `get_agent_events`, `get_agent_detail`, `list_agents`, `start_mission_execution`, `get_scheduler_status`, `list_agents_by_mission`, `get_default_workspace_path`, `list_agent_events`, `get_mission_cost_summary` | 11 |
| Review | `get_agent_diff`, `submit_review_action` | 2 |
| Intervention | `inject_agent_note`, `list_agent_notes`, `inject_mission_note`, `list_mission_notes` | 3 |

### 技术债务（Phase 0-2 遗留已清理情况）

| 原始债务 | 状态 | 处理方式 |
|---------|:---:|------|
| Agent workspace 路径硬编码 `/tmp/miragenty-workspace` | ✅ 已修复 | FM-02: `start_mission_execution` 接收用户选择的 `repo_path` |
| `stop_agent` 空实现 | ✅ 已修复 | FM-03: `AgentRegistry` + `CancellationToken` |
| Checkpoint 未持久化 | ✅ 已修复 | FM-03: `persist_event()` 写入 `agent_events` |
| `agent_events` 表未写入 | ✅ 已修复 | FM-03: 每步骤全部事件持久化 |
| 零测试覆盖 | ✅ 已修复 | 65 个 Rust UT + 2 个前端测试 |

### 新增技术债务

| 项目 | 严重度 | 说明 |
|------|:---:|------|
| 前端测试覆盖不足 | 中 | 仅 2 个测试文件，关键交互逻辑缺乏 UT |
| 无端到端集成测试 | 中 | 前后端联调缺乏自动化测试 |
| MissionsView 体积偏大 | 低 | 380 行，可拆分为更细粒度的子组件 |
| 未使用 react-router-dom | 低 | 已安装但视图切换仍用 Zustand 状态 |
| Insights/Agents 视图未实现 | 低 | 占位组件，Phase 2 需要仪表盘和 Agent 绩效时再做 |

### 下一步

**Phase 1 的核心循环验证已完成。** 推荐的后续路径：

1. **Phase 1.5: 测试补齐 + 集成验收**
   - 补充前端关键组件的 UT
   - 建立 Rust 集成测试（跨模块端到端场景）
   - 手动端到端验收：创建 Mission → Planner 分解 → 执行 → Review → 合并

2. **Phase 2: Harness 深化 + 核心差异化**（产品规划中定义的 10 项）
   - 独立 Evaluator Agent + Generator ↔ Evaluator 闭环
   - Mission Report + Trade-off 透明化
   - Pre-flight 需求澄清 Agent
   - Mission Contract
   - 成本预算系统 + 智能模型路由
   - 黄灯暂停 + 红灯重启
   - 推理链路追踪
   - DAG 可视化编排增强
