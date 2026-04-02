# FM-06: Runtime Intervention

> 版本: v1.0 | 日期: 2026-04-01  
> 优先级: P2 | 预估周期: 4-5 天  
> 依赖: FM-03 | 被依赖: 无

---

## IR — Initial Requirements

### IR-01: 用户故事

**US-01**: 作为开发者，我希望在 Agent 运行中给它补充约束，而不是只能等它跑完。

**US-02**: 作为开发者，我希望系统能在不粗暴中断当前 LLM 生成的前提下，在下一步自然注入我的新要求。

### IR-02: 业务价值

- 体现产品“主动操控而非被动旁观”的差异化
- 降低任务跑偏带来的返工成本

### IR-03: 高层验收标准

1. 支持对运行中 Agent 发送 breadcrumb 注入
2. 注入内容在下一个 checkpoint 后被 Agent 感知
3. UI 中可见已发送与已消费的介入指令

---

## SR — Software Requirements

### 功能需求

#### FR-01: Breadcrumb 注入

- **FR-01.1**: 新增 `inject_agent_note` command，参数 `{ agent_id, note }`
- **FR-01.2**: 注入内容写入待消费队列
- **FR-01.3**: Agent 在下一 checkpoint 读取并拼接到下一轮 system/context message
- **FR-01.4**: 注入后 UI 立即显示“queued”状态
- **FR-01.5**: 当 Agent 成功消费后，状态更新为“applied”

#### FR-02: 介入面板

- **FR-02.1**: Workspace 聚焦视图中提供 note 输入框
- **FR-02.2**: 支持对单个 Agent 发送注入
- **FR-02.3**: 支持查看最近 10 条注入记录

#### FR-03: 兼容取消与失败

- **FR-03.1**: 若 Agent 在消费前已结束，则该 note 状态为 `expired`
- **FR-03.2**: 若 Agent 被取消，未消费的 note 不再注入

### 非功能需求

- **NFR-01**: 注入操作本身不得阻塞 Agent 主循环
- **NFR-02**: 注入生效时间 ≤ 1 个 checkpoint 周期

### 接口需求

| Command | 参数 | 返回 | 说明 |
|---------|------|------|------|
| `inject_agent_note` | `{ agent_id, note }` | `{ note_id }` | 向运行中 Agent 注入便签条 |
| `list_agent_notes` | `{ agent_id }` | `AgentNote[]` | 查看注入历史 |

### 数据需求

建议新增表：

| 表 | 字段 | 用途 |
|----|------|------|
| `agent_notes` | id, agent_id, content, status, created_at, applied_at | 注入队列与审计 |

---

## AR — Architecture Requirements

### 设计说明

```text
用户输入 note
  -> inject_agent_note
  -> write agent_notes(status=queued)
  -> Agent checkpoint poll notes
  -> attach note to next request context
  -> mark applied
```

### 后端模块变更

| 文件 | 变更 |
|------|------|
| `agent/engine.rs` | checkpoint 读取待消费 note |
| `commands/agent.rs` | 注入与查询命令 |
| `db/migrations.rs` | 新增 `agent_notes` 表 |

### 前端模块变更

| 文件 | 变更 |
|------|------|
| `WorkspaceView.tsx` | 增加 Intervention Panel |
| `agent-store.ts` | 管理 note 历史 |

### 与其他模块交互

- **← FM-03**: 复用 checkpoint 和取消能力
- **← FM-04**: 在活动流中展示 note queued/applied/expired 事件
- **← FM-05**: review 反馈未来可复用本机制回注 Agent

### 现有代码关键入口

| 说明 | 文件路径 |
|------|---------|
| Agent checkpoint 位置 | `agent/engine.rs` 第 113-121 行（checkpoint emit 处是注入读取点） |
| 数据库迁移 | `db/migrations.rs`（需新增 `agent_notes` 表） |
| Workspace 聚焦视图 | `views/WorkspaceView.tsx`（需增加 note 输入区域） |

### Note 拼接格式

注入的 note 应以系统消息形式附加到下一轮 LLM 请求中：

```
[System Note - Priority Update from Commander]:
{note_content}

Please take this into account in your next steps.
```

多条 note 按 `created_at` 升序拼接，每条之间用空行分隔。

### `agent_notes` 表 DDL

```sql
CREATE TABLE IF NOT EXISTS agent_notes (
    id TEXT PRIMARY KEY,
    agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    content TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'queued'
        CHECK (status IN ('queued', 'applied', 'expired')),
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    applied_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_agent_notes_agent ON agent_notes(agent_id, status);
```
