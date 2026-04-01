# Dev Report #001: Foundation Phase 0-2

**日期**: 2026-03-31  
**范围**: 基础架构开发路线 Phase 0 / Phase 1 / Phase 2  
**代码量**: 3,009 行（Rust 1,303 / TS+TSX 951 / CSS 755）  
**Git 提交**: 2 commits on `main`

---

## 第一部分：概要

Phase 0-2 的目标是：在进入功能模块并行开发之前，搭建完整的基础设施。具体做了三件大事：

1. **Phase 0 — 项目脚手架**：从零初始化 Tauri 2.0 + React 19 + TypeScript 项目，建立完整的目录结构和开发工具链。
2. **Phase 1 — 核心基础设施**（三条并行轨道）：前端 Shell + 设计系统（Track A）、后端数据层 + LLM 客户端 + 配置系统（Track B）、前后端通信协议（Track C）。
3. **Phase 2 — Agent 执行引擎垂直切片**：实现了 Agent 步进执行循环、工具框架、实时事件推送，以及前端的活动流 UI，验证了端到端闭环可行性。

最终结果：`pnpm tauri dev` 可以一键启动 Tauri 桌面窗口，显示完整的 Commander Shell（侧边栏 + 标题栏 + 视图路由），Settings 页面可以配置 API Key，Workspace 页面可以输入任务指令并触发 Agent 执行。

---

## 第二部分：详细串讲

### Phase 0：项目脚手架

**做了什么**：
- 安装 pnpm、cargo-tauri CLI
- 用 `create-tauri-app` 生成 React + TypeScript 模板
- 定制项目名称（`Miragenty`）和窗口配置（1280x820、最小 900x600）

**目录结构**：

```
Miragenty/
├── src-tauri/           # Rust 后端
│   └── src/
│       ├── main.rs      # 入口（5行），调用 lib::run()
│       ├── lib.rs        # Tauri Builder 配置，注册所有 command
│       ├── commands/     # IPC command handlers（5个文件）
│       ├── agent/        # Agent 执行引擎（3个文件）
│       ├── db/           # SQLite 数据层（3个文件）
│       ├── llm/          # LLM API 客户端（4个文件）
│       ├── git/          # Git worktree 管理（2个文件）
│       └── tools/        # Agent 工具框架（3个文件）
├── src/                 # React 前端
│   ├── components/      # Sidebar, Titlebar, ui/（Button, Input, Badge）
│   ├── views/           # 5个视图页面
│   ├── hooks/           # useTheme
│   ├── stores/          # Zustand stores（ui, agent, task）
│   ├── ipc/             # Tauri IPC 封装（commands + events）
│   └── styles/          # CSS 变量 + 全局样式
├── design/prototypes/   # 7个 HTML 交互原型（Phase 0 之前产出）
└── reports/             # 开发报告（本文件）
```

**工具链**：
- 前端构建：Vite 7 + HMR
- 前端 Lint：ESLint 9 + typescript-eslint + react-hooks + react-refresh
- 前端格式化：Prettier 3
- 前端测试：Vitest 3 + Testing Library + jsdom
- 后端 Lint：Clippy + rustfmt
- 后端测试：Rust 内置 `#[test]`
- 开发模式：`pnpm tauri dev` 同时启动 Vite dev server + Cargo watch

**关键依赖**：

| 层 | 依赖 | 版本 | 用途 |
|---|-------|------|------|
| 桌面框架 | tauri | 2.10 | WebView 桌面应用 |
| 前端框架 | react + react-dom | 19.2 | UI 渲染 |
| 路由 | react-router-dom | 7.x | 视图路由（已安装，待接入） |
| 状态管理 | zustand | 5.x | 轻量全局 store |
| UI 原语 | @radix-ui/* | 1.x-2.x | headless 组件（Dialog, Tooltip, Tabs, Select 等） |
| 数据库 | rusqlite | 0.32 | SQLite（bundled 模式） |
| HTTP | reqwest | 0.12 | LLM API 调用 |
| Git | git2 | 0.19 | libgit2 Rust binding |
| 异步 | tokio | 1.x (full) | async runtime |
| 序列化 | serde + serde_json | 1.x | JSON 序列化 |

---

### Phase 1 Track A：前端基础

**Commander Shell（`App.tsx` + `Sidebar.tsx` + `Titlebar.tsx`）**：
- 经典三区布局：左侧 Sidebar（220px，backdrop-filter 模糊） + 顶部 Titlebar（52px，拖拽区域） + 主内容区
- Sidebar 导航 5 个视图：Missions / Workspace / Agents / Insights / Settings
- 视图通过 `useUiStore.activeView` 状态切换，不使用 URL 路由（单窗口桌面应用）

**设计系统（`styles/variables.css`）**：
- 遵循 Apple macOS 设计语言（`apple-design.mdc` 规则）
- CSS 变量体系：typography（SF Pro 系统字体栈，7 级字号）、spacing（8px 网格，12 级）、color（语义化颜色，light/dark 双套）、shadow（4 级）、radius（4 级）、animation（3 种 easing + 3 种 duration）
- Light/Dark/System 三模式：`prefers-color-scheme` media query + `data-theme` 属性覆盖
- Titlebar 上有主题循环切换按钮（sun/moon/monitor 图标）

**基础 UI 组件（`components/ui/`）**：
- `Button`：4 variants（primary/secondary/ghost/danger）× 3 sizes（sm/md/lg），forwardRef
- `Input`：带可选 label 和 error 提示，focus 时有 accent 边框 + box-shadow
- `Badge`：5 variants（default/success/warning/error/info），圆角胶囊样式

**Zustand Stores**：
- `ui-store`：activeView、theme、sidebarCollapsed + 对应 setters
- `agent-store`：agents Record，支持 add/update/remove/appendEvent
- `task-store`：missions + tasks Record，支持 CRUD

---

### Phase 1 Track B：后端基础

**SQLite 数据层（`db/`）**：
- `Database` 结构体：单连接 + `Mutex<Connection>`，WAL 模式，外键约束
- 版本化迁移系统：`schema_migrations` 表记录已执行迁移，启动时自动检查并执行
- 初始 schema（`001_initial`，6 张表）：

| 表 | 关键字段 | 用途 |
|---|---------|------|
| `missions` | id, title, description, status, total_cost_usd, created/updated_at | 任务总览 |
| `tasks` | id, mission_id(FK), title, status, assigned_agent_id | 子任务 |
| `task_dependencies` | task_id(FK), depends_on(FK) | DAG 依赖关系 |
| `agents` | id, name, task_id(FK), status, worktree_path, current_step, tokens_used, cost_usd | Agent 实例 |
| `agent_events` | id, agent_id(FK), kind, content, created_at | Agent 活动流 |
| `cost_records` | id, agent_id(FK), task_id(FK), model, input/output_tokens, cost_usd | Token 消耗 |

**LLM API 客户端（`llm/`）**：
- `LlmProvider` trait：`chat()` / `stream_chat()` / `estimate_cost()` 三个方法
- `AnthropicProvider` 实现：
  - 调用 `https://api.anthropic.com/v1/messages`
  - 同步模式：直接 POST 获取完整响应
  - 流式模式：解析 SSE 事件流（`content_block_delta` / `message_delta` / `message_start`），通过 `mpsc::Sender<StreamChunk>` 逐块发送
  - 成本估算：按模型名（opus/sonnet/haiku）区分每百万 token 费率
- 类型体系：`Message`、`ContentBlock`（Text/ToolUse/ToolResult）、`ToolDefinition`、`LlmRequest`、`LlmResponse`、`TokenUsage`、`StreamChunk`

**配置系统（`commands/config.rs`）**：
- `ConfigManager`：从 `{app_data_dir}/config.json` 读写
- 管理内容：API keys（HashMap）、default_model（默认 claude-sonnet-4-20250514）、max_concurrent_agents（默认 3）
- Tauri commands：`get_config` / `set_api_key` / `update_config`

---

### Phase 1 Track C：通信桥接

**Tauri Commands（前端 → 后端，共 9 个）**：

| 命令 | 参数 | 返回 | 用途 |
|------|------|------|------|
| `get_app_info` | — | `AppInfo` | 版本号 + 数据目录 |
| `get_db_status` | — | `String` | 已执行迁移数 |
| `create_mission` | `CreateMissionRequest` | `MissionInfo` | 创建任务 |
| `list_missions` | — | `Vec<MissionInfo>` | 列出所有任务 |
| `get_config` | — | `ConfigResponse` | 读取配置 |
| `set_api_key` | `SetApiKeyRequest` | `()` | 保存 API Key |
| `update_config` | `UpdateConfigRequest` | `()` | 更新配置 |
| `run_agent` | `RunAgentRequest` | `RunAgentResponse` | 启动 Agent |
| `stop_agent` | `agent_id` | `()` | 停止 Agent（占位） |

**Tauri Events（后端 → 前端推送，2 个事件名）**：
- `agent-event`：Agent 执行过程中的离散事件（llm_call / tool_use / tool_result / checkpoint / error / message）
- `agent-stream`：LLM 输出的实时文本流（TextDelta 类型的增量文本）

**前端 IPC 封装（`src/ipc/`）**：
- `commands.ts`：所有 `invoke()` 调用的类型安全封装，每个后端 command 对应一个函数
- `events.ts`：`listen()` 封装为 `onAgentEvent()` 和 `onAgentStream()`，返回 unlisten 函数
- `index.ts`：统一导出入口

---

### Phase 2：Agent 执行引擎垂直切片

**Agent Engine（`agent/engine.rs`，187 行，最核心的文件）**：
- 执行循环：`run(agent_id, task_description, max_steps)` → 循环直到完成或超限
- 每一步（Step）的流程：
  1. 构造 `LlmRequest`（system prompt + 累积 messages + tool definitions）
  2. 调用 `stream_chat()` 获取 LLM 响应，同时通过 `agent-stream` 事件实时推送文本
  3. 检查响应是否包含 `ToolUse`
  4. 如果有：逐个执行工具 → 记录结果 → 追加到 messages → 继续下一步
  5. 如果没有（纯文本回复）：视为任务完成
  6. 每步都通过 `agent-event` 推送 checkpoint（包含 token 消耗信息）
- 系统 prompt 模板：告诉 Agent 它的任务描述，指示它使用工具完成任务

**工具框架（`tools/`）**：
- 5 个内置工具：`read_file` / `write_file` / `search_files`（rg） / `shell_exec`（sh -c） / `list_files`
- 路径沙箱：`resolve_path()` 检查所有路径不能逃逸 workspace_root
- 每个工具都有完整的 JSON Schema 定义（用于 LLM function calling）

**Git Worktree（`git/worktree.rs`，92 行）**：
- 基于 `git2` crate（libgit2 binding）
- `create_worktree(agent_id)`：从 HEAD 创建分支 `agent/{id}` + 独立工作树 `.worktrees/{id}`
- `remove_worktree(agent_id)`：清理工作树目录 + 删除分支
- `get_diff(agent_id)`：获取 worktree 的 patch 格式 diff
- 注意：当前 Agent command 使用硬编码 `/tmp/miragenty-workspace`，尚未接入 worktree 隔离

**前端活动流 UI（`views/WorkspaceView.tsx`，197 行）**：
- 顶部输入栏：文本输入 + "Run Agent" 按钮
- Agent 标签页：多 Agent 切换，每个标签显示 agent ID 前 6 位 + 状态 Badge（running/completed/failed）+ 脉动圆点
- 活动流面板：
  - 离散事件卡片：每个步骤显示 Badge（step 号 + kind）+ 等宽字体内容
  - 流式文本块：蓝色边框卡片 + 闪烁光标
  - 自动滚动到底部
- 订阅两个后端事件：`agent-event`（结构化步骤）+ `agent-stream`（文本增量）

**Settings UI（`views/SettingsView.tsx`，82 行）**：
- API Keys 区域：Anthropic key 输入框 + 保存按钮 + 状态 Badge（Configured/Not Set）
- Model 区域：显示当前默认模型名
- Agents 区域：显示最大并发数
- 调用 `get_config` / `set_api_key` 后端命令

---

### 额外完成项

- **DevTools 自动打开**：`#[cfg(debug_assertions)]` 下自动打开 WebView Inspector
- **组件标识**：主要布局元素加 `data-component` 属性，方便 UI 问题定位
- **Git 仓库**：已初始化，2 个提交

---

## 第三部分：How Far Are We

### 对照产品 Phase 1 清单（9 项"必须有"）

| # | 功能 | 基础设施就绪 | 功能开发状态 | 说明 |
|---|------|:---:|:---:|------|
| 1 | 对话面板：输入需求，Planner 分解任务 | **是** | **20%** | Workspace 有输入栏，但尚无 Planner prompt / 任务拆解逻辑 |
| 2 | 2-3 个 Agent 并行执行（worktree 隔离） | **是** | **30%** | Agent 引擎可运行，WorktreeManager 已实现，但未接入引擎 + 无并发调度器 |
| 3 | 步进式执行引擎 + Harness 检查点 | **是** | **60%** | 步进循环已实现、事件推送已通、checkpoint 存在但未持久化到 SQLite |
| 4 | 基础 Schema 验证（编译/类型检查） | **否** | **0%** | 需要在 tool 执行后加验证步骤 |
| 5 | Agent 活动实时流 | **是** | **70%** | WorkspaceView 可显示事件流和文本流，缺少步骤进度条 |
| 6 | 统一 Code Diff 审查 | **是** | **0%** | git2 get_diff 已实现，前端 Monaco 未集成 |
| 7 | Per-Task 成本追踪 | **是** | **30%** | cost_records 表已建，LLM 有 token 统计，但前端未展示 |
| 8 | 便签条注入（运行时介入） | **是** | **0%** | 需要 checkpoint 暂停 + 消息注入机制 |
| 9 | 基础 DAG 编辑 | **是** | **0%** | task_dependencies 表已建，需调度器 + UI |

### 进度矩阵

```
基础设施  ████████████████████ 100%  (Phase 0-2 完成)
功能模块  ██░░░░░░░░░░░░░░░░░░  ~20%  (仅 Agent 引擎骨架 + 活动流)
测试覆盖  ░░░░░░░░░░░░░░░░░░░░   0%  (Vitest/cargo test 已配置，无测试用例)
```

### 下一步（按建议优先级）

1. **P0: Planner 任务分解** — 接入 LLM 做任务拆解，存入 tasks 表，前端对话面板
2. **P0: 多 Agent 并行** — Agent 调度器 + WorktreeManager 接入 + 并发 worktree
3. **P1: Code Diff 审查** — Monaco Editor 集成 + worktree diff 展示
4. **P1: 成本追踪 UI** — 实时 token/cost 指示器
5. **P2: 运行时介入** — Checkpoint 持久化 + 便签条注入

### 技术债务

| 项目 | 严重度 | 说明 |
|------|:---:|------|
| Agent workspace 路径硬编码 | 高 | `run_agent` 使用 `/tmp/miragenty-workspace`，需要改为用户选择的项目目录 |
| Agent 无法取消 | 中 | `stop_agent` 是空实现，需要 `CancellationToken` |
| Checkpoint 未持久化 | 中 | 当前仅推送事件，未写入 `agent_events` 表 |
| 未使用的 Cargo 依赖 | 低 | `dirs`、`thiserror`、`chrono`、`tracing-appender` 已声明但未引用 |
| 零测试覆盖 | 中 | Vitest 和 cargo test 已配置但无测试用例 |
| react-router-dom 未使用 | 低 | 已安装但视图切换用 Zustand 状态而非 URL 路由 |
