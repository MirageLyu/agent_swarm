# FM-14: Approval Queue

> 版本: v1.0 | 日期: 2026-04-08  
> 优先级: P1 | 预估周期: 5-7 天  
> 依赖: FM-02（Agent 调度）、FM-03（Agent 暂停机制）、FM-09（TopBar / Sidebar UI 骨架） | 被依赖: 无  
> 原型参考: `design/prototypes/01-commander-shell.html`（底部 Approval Queue 区域）

---

## IR — Initial Requirements

### IR-01: 用户故事

**US-01**: 作为开发者，我希望 Agent 在执行破坏性操作（删除文件、安装依赖、修改配置等）前自动暂停并请求我的审批，这样危险操作不会在我不知情的情况下执行。

**US-02**: 作为开发者，我希望所有待审批请求汇总在一个队列中，我可以逐一或批量处理，这样审批效率高。

**US-03**: 作为开发者，我希望在 TopBar 和 Sidebar 看到待审批计数 Badge，这样我不会错过审批请求。

### IR-02: 业务价值

- **安全控制**：Agent 自主操作的最后一道防线
- **信任建立**：用户感知到系统的安全边界
- **差异化**：精细的权限控制是 Harness 工程的核心能力之一
- **可扩展**：审批机制为后续自定义策略引擎打基础

### IR-03: 高层验收标准

1. Agent 执行破坏性 tool 前自动暂停，创建审批请求
2. 主界面底部 Approval Queue 栏显示所有待审批项
3. 每项显示 Agent 名、操作描述、Approve / Reject 按钮
4. 支持 Approve All / Reject All 批量操作
5. TopBar 和 Sidebar 显示待审批计数 Badge
6. Approve 后 Agent 自动继续执行
7. Reject 后 Agent 跳过该操作或终止

---

## SR — Software Requirements

### 功能需求

#### FR-01: 审批触发机制

- **FR-01.1**: Tool Executor 中为以下操作添加审批拦截：
  - `write_file`：目标文件在 `protected_paths` 列表中（如 `package.json`, `Cargo.toml`, `*.config.*`）
  - `run_command`：命令匹配 `destructive_commands` 模式（如 `rm -rf`, `npm install`, `git push`, `DROP TABLE`）
  - 任何操作的成本累计超过 Contract 预算的 80%
- **FR-01.2**: 拦截时 Agent 执行暂停（使用 FM-03 的 CancellationToken 暂停机制），创建 `approval_requests` 记录
- **FR-01.3**: 暂停的 Agent 状态变为 `waiting_approval`
- **FR-01.4**: 通过 `approval-requested` 事件通知前端

#### FR-02: 审批请求数据模型

- **FR-02.1**: 每个请求包含：agent_id、tool_name、tool_input（操作参数）、reason（为什么需要审批）、context_summary（当前执行上下文摘要）
- **FR-02.2**: 请求状态：`pending` → `approved` / `rejected` / `expired`
- **FR-02.3**: 超时机制：请求创建后 10 分钟未处理自动标记 `expired`，Agent 跳过该操作继续

#### FR-03: Approval Queue UI

- **FR-03.1**: 主内容区底部新增 Approval Queue 栏（140px 高），仅在有 pending 请求时显示
- **FR-03.2**: 顶部：标题"Approval Queue" + 橙色计数 Badge + "Approve All" / "Reject All" 按钮
- **FR-03.3**: 内容区：横向可滚动的卡片列表（`overflow-x: auto`）
- **FR-03.4**: 每张卡片（min-width 260px）包含：
  - Header：状态圆点 + Agent 名（粗体）
  - Body：等宽字体显示操作描述（如 `delete src/models/user_old.ts`）
  - Footer：Approve（绿色）/ Reject（红色）按钮
- **FR-03.5**: Approve 按钮 hover 时填绿色、Reject 按钮 hover 时填红色

#### FR-04: 审批操作

- **FR-04.1**: Approve：调用 `resolve_approval` command（action=approve），Agent 恢复执行该 tool
- **FR-04.2**: Reject：调用 `resolve_approval` command（action=reject），Agent 跳过该 tool 并记录一条 skip 事件
- **FR-04.3**: Approve All：批量 approve 所有 pending 请求
- **FR-04.4**: Reject All：批量 reject 所有 pending 请求
- **FR-04.5**: 操作后卡片从队列中移除（淡出动画）

#### FR-05: Badge 联动

- **FR-05.1**: TopBar 的 `TopBarMetrics` 组件中增加审批计数 Badge（橙色）
- **FR-05.2**: Sidebar 中对应 Agent 项显示"等待审批"状态（橙色状态点）
- **FR-05.3**: Badge 通过监听 `approval-requested` / `approval-resolved` 事件实时更新

#### FR-06: 审批策略配置

- **FR-06.1**: Settings 中新增 "Approval Policy" 配置节：
  - Protected Paths：文本列表，可增删
  - Destructive Commands：模式列表，可增删
  - Auto-approve trusted tools：开关（默认关）
- **FR-06.2**: 策略保存到 `config.json`

### 非功能需求

- **NFR-01**: Agent 暂停到前端显示审批卡片 ≤ 500ms
- **NFR-02**: 审批操作到 Agent 恢复执行 ≤ 200ms
- **NFR-03**: Queue 中 20 个卡片时横向滚动流畅
- **NFR-04**: 超时自动过期在 ±10 秒内准确

### 接口需求

新增 Tauri Commands：

| Command | 参数 | 返回 | 说明 |
|---------|------|------|------|
| `list_pending_approvals` | `{ mission_id? }` | `ApprovalRequest[]` | 获取待审批列表 |
| `resolve_approval` | `{ request_id, action }` | `()` | 审批/拒绝 |
| `resolve_all_approvals` | `{ action }` | `{ resolved_count }` | 批量操作 |
| `get_approval_policy` | `()` | `ApprovalPolicy` | 获取审批策略 |
| `update_approval_policy` | `ApprovalPolicy` | `()` | 更新审批策略 |

新增 Tauri Events：

| Event | Payload | 说明 |
|-------|---------|------|
| `approval-requested` | `ApprovalRequest` | Agent 请求审批 |
| `approval-resolved` | `{ request_id, action }` | 审批已处理 |

### 数据需求

新增 Schema 迁移：

```sql
CREATE TABLE IF NOT EXISTS approval_requests (
    id TEXT PRIMARY KEY,
    mission_id TEXT NOT NULL REFERENCES missions(id) ON DELETE CASCADE,
    agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    tool_name TEXT NOT NULL,
    tool_input TEXT NOT NULL DEFAULT '{}',
    reason TEXT NOT NULL,
    context_summary TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'approved', 'rejected', 'expired')),
    resolved_at TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX idx_approval_pending ON approval_requests(status)
    WHERE status = 'pending';
```

`agents.status` CHECK 约束需追加 `'waiting_approval'` 值。

---

## AR — Architecture Requirements

### 前端组件设计

| 组件 | 路径 | 职责 |
|------|------|------|
| `ApprovalQueue` | `src/components/ApprovalQueue.tsx` | 底部审批队列栏容器 |
| `ApprovalCard` | `src/components/ApprovalCard.tsx` | 单个审批卡片 |
| `ApprovalBadge` | `src/components/ApprovalBadge.tsx` | TopBar/Sidebar 审批计数 Badge |
| `ApprovalPolicyEditor` | `src/components/settings/ApprovalPolicyEditor.tsx` | Settings 中审批策略编辑器 |

### 后端模块变更

| 文件 | 变更 |
|------|------|
| `tools/executor.rs` | 增加审批拦截逻辑：检查 protected paths / destructive commands |
| `commands/approval.rs`（新） | 审批相关 commands |
| `agent/engine.rs` | 支持 `waiting_approval` 状态：暂停执行循环、等待审批信号 |
| `agent/scheduler.rs` | 处理审批超时过期 |
| `db/migrations.rs` | 新增 approval_requests 表、agents status 扩展 |
| `commands/config.rs` | 支持 approval_policy 配置读写 |

### 数据流

```text
Agent 执行 tool_use
  → ToolExecutor 检查审批策略
  → 匹配到 protected/destructive
      → 创建 approval_request(pending)
      → Agent status → waiting_approval
      → 发射 approval-requested 事件
      → 前端 ApprovalQueue 显示卡片

用户点击 Approve
  → resolve_approval(approve)
  → approval_request.status → approved
  → Agent 恢复执行该 tool
  → Agent status → running
  → 发射 approval-resolved 事件
  → 卡片从队列移除

用户点击 Reject
  → resolve_approval(reject)
  → Agent 跳过该 tool、记录 skip 事件
  → Agent 继续下一步

超时 10 分钟
  → Scheduler 检测过期
  → approval_request.status → expired
  → Agent 跳过该 tool、继续执行
```

### 与其他模块的交互

- **← FM-03**: 复用 CancellationToken 的暂停机制
- **← FM-06**: 与 Breadcrumb 注入互补——Breadcrumb 是主动介入，Approval 是被动请求
- **← FM-09**: TopBar Badge 和 Sidebar 状态点
- **→ FM-13**: 审批统计数据纳入 Anomalies 面板

### 现有代码关键入口

| 说明 | 文件路径 |
|------|---------|
| Tool Executor | `src-tauri/src/tools/executor.rs` |
| Tool 定义 | `src-tauri/src/tools/definitions.rs` |
| Agent 引擎步进循环 | `src-tauri/src/agent/engine.rs` |
| CancellationToken 注册 | `src-tauri/src/agent/registry.rs` |
| TopBar Metrics | `src/components/TopBarMetrics.tsx`（FM-09） |
| Sidebar Agent 列表 | `src/components/SidebarAgentList.tsx`（FM-09） |
| Settings 视图 | `src/views/SettingsView.tsx` |
| Config 管理 | `src-tauri/src/commands/config.rs` |
