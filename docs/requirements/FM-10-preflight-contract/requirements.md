# FM-10: Pre-flight & Mission Contract

> 版本: v1.0 | 日期: 2026-04-08  
> 优先级: P0 | 预估周期: 7-10 天  
> 依赖: FM-01（Planner Agent 基础）、FM-09（UI 骨架） | 被依赖: FM-11, FM-12  
> 原型参考: `design/prototypes/02-preflight-chat.html`

---

## IR — Initial Requirements

### IR-01: 用户故事

**US-01**: 作为开发者，我希望在提交需求后，先与 Planner Agent 进行多轮对话式澄清，这样可以在执行前暴露歧义、遗漏和风险。

**US-02**: 作为开发者，我希望对话按三个阶段（场景走查 → 魔鬼代言人 → 风险标记）引导我思考，这样不同角度的问题都能被覆盖。

**US-03**: 作为开发者，我希望对话过程中实时构建一份结构化的 Mission Contract（范围、约束、验收标准、风险项），这样澄清结果不是散落的聊天记录而是正式文档。

**US-04**: 作为开发者，我希望在签署 Contract 后，系统基于 Contract 自动生成 Task DAG，这样规划质量比单轮 Planner 更高。

### IR-02: 业务价值

- **降低返工率**：执行前澄清需求，减少 Agent 理解偏差导致的无效输出
- **建立信任**：Contract 机制让用户感受到对需求的尊重和控制力
- **提升规划质量**：多轮对话产出的 Contract 包含验收标准，为 FM-11 Evaluator 提供评判基准
- **差异化**：Pre-flight 是 Cursor/Copilot 等产品不具备的流程创新

### IR-03: 高层验收标准

1. 用户输入需求 → 进入 Pre-flight 对话界面
2. 三阶段模式可切换，每阶段 Agent 提出不同角度的问题
3. Agent 提供选项式问题，用户点选或自由回复
4. 对话右侧实时更新 Mission Contract 四个分区
5. Contract 底部可配置预算/质量门槛/最大时长
6. 签署 Contract → 自动调用 Planner 生成 DAG → 进入 MissionsView

---

## SR — Software Requirements

### 功能需求

#### FR-01: Pre-flight 对话面板

- **FR-01.1**: MissionsView 中用户输入需求后，可选择"Quick Plan"（沿用 FM-01 单轮）或"Pre-flight"（进入多轮澄清）
- **FR-01.2**: Pre-flight 对话界面为双栏布局：左侧聊天面板（420px）、右侧 Contract 面板（flex:1）
- **FR-01.3**: 聊天面板顶部显示模式切换分段控件：场景走查 / 魔鬼代言人 / 风险标记
- **FR-01.4**: 聊天区域支持 Agent 消息（灰底左对齐）和用户消息（蓝底右对齐）
- **FR-01.5**: Agent 消息出现前显示 typing indicator（三点跳动动画）
- **FR-01.6**: 底部输入框支持 Enter 发送、Shift+Enter 换行

#### FR-02: 多轮 Planner Agent — 对话模式

- **FR-02.1**: 后端新增 `start_preflight` Tauri command：创建 Mission（status=`preflight`）、初始化对话 session
- **FR-02.2**: 后端新增 `send_preflight_message` Tauri command：接收用户消息、追加到对话历史、调用 LLM、返回 Agent 回复
- **FR-02.3**: LLM 调用使用 streaming 模式，通过 `preflight-stream` 事件推送到前端
- **FR-02.4**: System prompt 按模式切换：
  - 场景走查：引导用户描述使用场景、用户角色、数据流
  - 魔鬼代言人：质疑用户假设，提出反例和极端情况
  - 风险标记：识别技术风险、依赖风险、安全风险
- **FR-02.5**: Agent 回复可包含 `choices` 数组（结构化选项），前端渲染为可点击按钮
- **FR-02.6**: 用户点选 choice 后，前端将选项文本作为用户消息发送，同时高亮选中选项、灰显其余

#### FR-03: 选项式交互

- **FR-03.1**: Agent 回复 JSON 中可包含 `choices: [{id, label, contract_impact}]` 字段
- **FR-03.2**: `contract_impact` 标记此选项会向 Contract 的哪个分区（scope/constraints/acceptance/risks）添加哪个条目
- **FR-03.3**: 用户选择后，前端自动调用 `add_contract_item` 将条目追加到 Contract
- **FR-03.4**: 用户也可自由输入文字回复，不强制选择

#### FR-04: Mission Contract 面板

- **FR-04.1**: 右侧面板分四个区块，每块有图标、标题和计数：
  - ✓ 用户明确要求的（Scope）
  - ◆ Agent 自主决策的（Constraints / Agent Decisions）
  - ✕ 明确不做的（Exclusions）
  - ○ 已确认的环境前提（Assumptions）
- **FR-04.2**: 每个条目新增时带"NEW"标签，2 秒后淡出
- **FR-04.3**: 条目支持手动删除（×按钮）
- **FR-04.4**: Contract 底部三项配置卡：
  - 预算上限（$，数字输入）
  - 质量门槛（/10，数字输入）
  - 最大时长（小时，数字输入）
- **FR-04.5**: 配置变更实时保存到后端 Contract 数据

#### FR-05: Contract 签署与 DAG 生成

- **FR-05.1**: Contract 底部"签署合同并启动 Swarm"按钮，仅在 Contract 至少有 1 个 Scope 条目时可用
- **FR-05.2**: 点击签署 → 后端调用 `sign_contract` command：
  - 将 Contract 序列化存入 `mission_contracts` 表
  - 基于 Contract 内容调用 Planner Agent 生成 Task DAG（system prompt 包含 Contract 全文作为约束）
  - Mission status 变为 `planned`
- **FR-05.3**: DAG 生成完成后自动跳转 MissionsView DAG 视图
- **FR-05.4**: 签署后 Contract 变为只读，可在 Mission 详情中查看

#### FR-06: 底部状态栏

- **FR-06.1**: 对话底部显示状态栏：左侧状态点 + 文案（"Pre-flight 进行中"）、右侧进度条 + 百分比
- **FR-06.2**: 进度基于对话轮数估算（每模式 3-5 轮为 100%）
- **FR-06.3**: 100% 完成时显示提示文案（"澄清完成，可签署 Contract"）

### 非功能需求

- **NFR-01**: 对话 streaming 延迟 ≤ 500ms 首 token
- **NFR-02**: Contract 条目更新实时反映，延迟 ≤ 100ms
- **NFR-03**: 对话历史上限 50 轮（单 session），超出时自动压缩早期对话

### 接口需求

新增 Tauri Commands：

| Command | 参数 | 返回 | 说明 |
|---------|------|------|------|
| `start_preflight` | `{ description }` | `{ mission_id, session_id }` | 创建 Mission + Pre-flight session |
| `send_preflight_message` | `{ session_id, message, mode }` | streaming via event | 发送用户消息，Agent 流式回复 |
| `add_contract_item` | `{ mission_id, section, text }` | `()` | 向 Contract 添加条目 |
| `remove_contract_item` | `{ mission_id, item_id }` | `()` | 从 Contract 移除条目 |
| `update_contract_config` | `{ mission_id, budget?, quality_threshold?, max_duration_hours? }` | `()` | 更新 Contract 配置 |
| `get_contract` | `{ mission_id }` | `Contract` | 获取完整 Contract |
| `sign_contract` | `{ mission_id }` | `PlanMissionResponse` | 签署 + 生成 DAG |

新增 Tauri Events：

| Event | Payload | 说明 |
|-------|---------|------|
| `preflight-stream` | `{ session_id, chunk }` | Pre-flight Agent 流式输出 |

### 数据需求

新增 Schema 迁移：

```sql
CREATE TABLE IF NOT EXISTS mission_contracts (
    id TEXT PRIMARY KEY,
    mission_id TEXT NOT NULL REFERENCES missions(id) ON DELETE CASCADE,
    status TEXT NOT NULL DEFAULT 'drafting'
        CHECK (status IN ('drafting', 'signed')),
    budget_usd REAL,
    quality_threshold REAL,
    max_duration_hours REAL,
    signed_at TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS contract_items (
    id TEXT PRIMARY KEY,
    contract_id TEXT NOT NULL REFERENCES mission_contracts(id) ON DELETE CASCADE,
    section TEXT NOT NULL
        CHECK (section IN ('scope', 'constraints', 'exclusions', 'assumptions')),
    text TEXT NOT NULL,
    source TEXT NOT NULL DEFAULT 'user'
        CHECK (source IN ('user', 'agent')),
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS preflight_sessions (
    id TEXT PRIMARY KEY,
    mission_id TEXT NOT NULL REFERENCES missions(id) ON DELETE CASCADE,
    mode TEXT NOT NULL DEFAULT 'scenario_walk'
        CHECK (mode IN ('scenario_walk', 'devils_advocate', 'risk_highlighter')),
    messages TEXT NOT NULL DEFAULT '[]',
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
```

`missions.status` CHECK 约束需追加 `'preflight'` 值。

---

## AR — Architecture Requirements

### 前端组件设计

| 组件 | 路径 | 职责 |
|------|------|------|
| `PreflightView` | `src/views/PreflightView.tsx` | 双栏容器：聊天 + Contract |
| `PreflightChat` | `src/components/preflight/PreflightChat.tsx` | 聊天消息列表 + 输入框 |
| `PreflightModeSwitch` | `src/components/preflight/PreflightModeSwitch.tsx` | 三模式分段控件 |
| `ChatMessage` | `src/components/preflight/ChatMessage.tsx` | 单条消息（用户/Agent） |
| `ChoiceButtons` | `src/components/preflight/ChoiceButtons.tsx` | Agent 选项按钮组 |
| `ContractPanel` | `src/components/preflight/ContractPanel.tsx` | Contract 四区块 + 配置 |
| `ContractSection` | `src/components/preflight/ContractSection.tsx` | 单个 Contract 区块 |
| `ContractConfigCards` | `src/components/preflight/ContractConfigCards.tsx` | 底部三项配置卡 |
| `PreflightStatusBar` | `src/components/preflight/PreflightStatusBar.tsx` | 底部进度条 |

### 后端模块变更

| 文件 | 变更 |
|------|------|
| `agent/planner.rs` | 新增 `preflight_chat()` 支持多轮对话模式，按 mode 切换 system prompt |
| `commands/preflight.rs`（新） | Pre-flight 相关 commands |
| `commands/mod.rs` | 注册新 commands |
| `db/migrations.rs` | 新增迁移：`mission_contracts`、`contract_items`、`preflight_sessions`、missions status 扩展 |
| `lib.rs` | 注册新 commands |

### 数据流

```text
用户输入需求 → 选择 "Pre-flight"
  → start_preflight → 创建 Mission(preflight) + Contract(drafting) + Session
  → 对话循环：
      send_preflight_message → LLM streaming → preflight-stream 事件
      → Agent 回复含 choices → 用户选择 → add_contract_item
  → sign_contract
      → Contract(signed) + Planner(含 Contract 约束) → Task DAG
      → Mission(planned) → 跳转 MissionsView
```

### 与其他模块的交互

- **← FM-01**: 复用 Planner Agent 基础能力（LLM 调用、DAG 校验逻辑）
- **→ FM-11**: Contract 中的 Acceptance Criteria 作为 Evaluator 的评判标准
- **→ FM-12**: Contract 与实际执行的对比是 Mission Report 的核心内容

### 现有代码关键入口

| 说明 | 文件路径 |
|------|---------|
| Planner Agent 实现 | `src-tauri/src/agent/planner.rs` |
| LLM 流式调用 | `src-tauri/src/llm/provider.rs` + `openai_compat.rs` |
| Mission CRUD | `src-tauri/src/commands/mission.rs` |
| 前端 IPC 封装 | `src/ipc/commands.ts`, `src/ipc/events.ts` |
| MissionsView（Pre-flight 入口） | `src/views/MissionsView.tsx` |
| 设计系统 | `src/styles/variables.css` |
