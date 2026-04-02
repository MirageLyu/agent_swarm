# FM-03: Execution Engine Hardening

> 版本: v1.0 | 日期: 2026-04-01  
> 优先级: P0 | 预估周期: 4-5 天  
> 依赖: 无 | 被依赖: FM-04, FM-06

---

## IR — Initial Requirements

### IR-01: 用户故事

**US-01**: 作为开发者，我希望 Agent 的每一步执行状态都能被持久化，这样应用重启后我仍能看到执行历史。

**US-02**: 作为开发者，我希望系统在工具调用后自动做基础验证，例如命令退出码、类型检查或文件存在性检查，这样能尽早发现明显错误。

**US-03**: 作为开发者，我希望能停止一个失控的 Agent，这样不会持续浪费 token 和时间。

### IR-02: 业务价值

- 提升 Agent 引擎稳定性和可恢复性
- 为运行时介入、活动回放、审计留好数据地基
- 减少“黑盒执行”带来的失控风险

### IR-03: 高层验收标准

1. Agent 每步 checkpoint 和关键事件落库
2. 工具执行后具备最小可用的 schema/结果校验
3. `stop_agent` 可真正生效
4. 应用重启后能恢复已记录的 Agent 历史

---

## SR — Software Requirements

### 功能需求

#### FR-01: Checkpoint 持久化

- **FR-01.1**: 每次 LLM 调用完成后写入一条 `agent_events`
- **FR-01.2**: 每次 tool use、tool result、error、checkpoint 都写入 `agent_events`
- **FR-01.3**: 每条事件至少包含 `agent_id`, `kind`, `content`, `created_at`
- **FR-01.4**: 前端实时事件推送与数据库写入必须同时发生，二者内容保持一致

#### FR-02: 基础 Schema 验证

> **Phase 1 范围限定**：仅做 exit code 标准化 + 工具错误结构化。不做编译检查、类型检查、AST 分析等复杂静态验证。文件路径越界检查已在现有 `executor.rs` 的 `resolve_path` 中实现，复用即可。

- **FR-02.1**: `shell_exec` 工具返回值必须包含 `exit_code` 字段（整数）
- **FR-02.2**: 非 0 exit code 的返回必须设置 `is_error: true`，并将 stderr 内容包含在错误信息中
- **FR-02.3**: 所有工具（`read_file`、`write_file`、`list_files`、`search_files`、`shell_exec`）失败时返回统一的错误结构：`{ "error": "<类型>", "message": "<详情>" }`
- **FR-02.4**: 工具返回的错误信息会被 Agent 在下一步 LLM 调用中看到（通过 `ToolResult.is_error = true`），无需额外验证步骤

#### FR-03: Agent 取消

- **FR-03.1**: `stop_agent(agent_id)` 不再是占位实现
- **FR-03.2**: 每个 Agent 运行时绑定 `CancellationToken`
- **FR-03.3**: 在每个 checkpoint 检查是否收到取消信号
- **FR-03.4**: 被取消的 Agent 状态更新为 `cancelled`
- **FR-03.5**: 取消后不再继续调用 LLM 或工具

#### FR-04: Agent 恢复视图

- **FR-04.1**: 新增 `get_agent_events(agent_id)` command
- **FR-04.2**: Workspace 页面支持从数据库回放历史事件，而不只依赖运行时事件流
- **FR-04.3**: 若应用重启，仍可查看已完成或失败 Agent 的完整活动记录

### 非功能需求

- **NFR-01**: 事件落库不能显著拖慢执行，单条写入目标 < 20ms
- **NFR-02**: Agent 取消请求生效时间 ≤ 1 个 checkpoint 周期
- **NFR-03**: 验证器错误不能导致应用崩溃，只能转为 Agent 错误事件

### 接口需求

新增/完善 Commands：

| Command | 参数 | 返回 | 说明 |
|---------|------|------|------|
| `stop_agent` | `{ agent_id }` | `()` | 取消运行中 Agent |
| `get_agent_events` | `{ agent_id }` | `AgentEvent[]` | 查询历史事件 |
| `get_agent_detail` | `{ agent_id }` | `AgentDetail` | 查询 Agent 状态、当前 step、tokens 等 |

### 数据需求

- `agent_events` 成为真实事件源，不再只是预留表
- `agents.status` 补齐 `cancelled`
- `agents.current_step` 每步更新

---

## AR — Architecture Requirements

### 组件设计

```text
AgentEngine step
   ├─ emit event
   ├─ persist event
   ├─ validate result
   ├─ checkpoint
   └─ check cancellation token
```

### 后端模块变更

| 文件 | 变更 |
|------|------|
| `src-tauri/src/agent/engine.rs` | 增加事件落库、取消检查、验证器挂点 |
| `src-tauri/src/commands/agent.rs` | 实现 stop_agent / get_agent_events |
| `src-tauri/src/tools/executor.rs` | 标准化工具错误结构和 exit code 返回 |
| `src-tauri/src/db/*` | 事件写入、Agent 状态更新、查询接口 |

### 关键设计决策

1. **事件双写**：运行时推送给前端，同时写 SQLite
2. **取消点只放在 checkpoint**：不试图中断正在进行的 LLM HTTP 请求
3. **先做基础验证器**：本阶段不做复杂静态分析，只做低成本高收益校验

### 时序图

```text
Agent step
  -> LLM response
  -> persist llm event
  -> execute tool
  -> persist tool_result
  -> validate
  -> persist checkpoint
  -> check cancellation
  -> next step / stop
```

### 与其他模块交互

- **→ FM-04**: 活动流读取历史事件并展示验证结果
- **→ FM-06**: 取消、checkpoint、事件落库是运行时介入前提
- **← FM-02**: 调度器依赖 Agent 终态推进任务

### 现有代码关键入口

| 说明 | 文件路径 |
|------|---------|
| Agent 引擎主循环（**核心改造对象**） | `src-tauri/src/agent/engine.rs`（187 行） |
| 现有 `emit_event` 辅助方法 | `engine.rs` 第 177-187 行，当前仅推送不落库 |
| 现有 `stop_agent` 空实现 | `src-tauri/src/commands/agent.rs` 第 76-78 行 |
| 工具执行器 | `src-tauri/src/tools/executor.rs` |
| 数据库连接 | `src-tauri/src/db/pool.rs` → `Database::with_conn()` |
| agent_events 表定义 | `src-tauri/src/db/migrations.rs` 第 52-59 行 |

### 实现提示

- **CancellationToken**: 推荐使用 `tokio_util::sync::CancellationToken`。在 `run_agent` 创建 token，存入全局 `HashMap<String, CancellationToken>`（用 `Arc<Mutex<...>>`），`stop_agent` 通过 agent_id 查找并 cancel。
- **事件落库**: `AgentEngine` 需要接收 `Database` 引用。在 `emit_event` 中同时 emit + insert。
- **验证器**: 建议在 `tools/executor.rs` 的 `execute` 返回值中增加结构化的成功/失败信息，而非只返回 `String`。
