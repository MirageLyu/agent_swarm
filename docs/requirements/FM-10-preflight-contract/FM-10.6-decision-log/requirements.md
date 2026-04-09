# FM-10.6: Decision Log — 决策日志

> 版本: v1.0 | 日期: 2026-04-09  
> 优先级: **P2** | 预估周期: 1 天  
> 依赖: FM-10.1 (Tool-as-Structure), FM-10.2 (Belief State) | 被依赖: FM-10.5 (Compaction 可引用决策日志)  
> 调研来源: Claude Code 架构分析 §模式6 (Layered Memory); Anthropic Context Engineering (2025.09)

---

## 1. 目标

建立 Pre-flight 对话中每个关键决策的结构化记录，解决：

1. **不可追溯**：当前 Contract 条目只记录"是什么"，不记录"为什么这样决定"
2. **重复提问**：LLM 不知道哪些方案已被否决，可能反复建议
3. **Compaction 信息丢失**：压缩后决策理由消失，Agent 无法回答"为什么"类问题
4. **签署前回顾困难**：用户在签署 Contract 前无法快速回顾决策路径

---

## 2. 现状分析

| 维度 | 当前实现 | 问题 |
|------|----------|------|
| 决策记录 | 隐含在对话历史中 | 无结构化索引，难以检索 |
| 决策理由 | 仅存在于 Agent 对话文本 | 压缩后丢失 |
| 被否决方案 | 无记录 | LLM 可能重复建议已被否决的方案 |
| 决策变更 | `update_contract_item` 覆盖旧内容 | 无变更历史 |

---

## 3. 功能需求

### FR-10.6.1: DecisionLog 数据结构

```rust
struct DecisionEntry {
    id: String,                    // UUID
    round: u32,                    // 发生轮次
    decision_type: DecisionType,   // 决策类型
    description: String,           // 决策内容
    rationale: String,             // 决策理由
    alternatives: Vec<Alternative>, // 被否决的替代方案
    contract_item_id: Option<String>, // 关联的 Contract 条目 ID
    created_at: DateTime,
}

enum DecisionType {
    Confirmed,       // 用户明确确认
    Rejected,        // 用户明确否决
    Inferred,        // Agent 推断（用户未反对）
    Revised,         // 修改了之前的决策
    Skipped,         // 用户跳过（"你决定"）
}

struct Alternative {
    label: String,           // 方案名称
    reason_rejected: String, // 否决原因
}
```

### FR-10.6.2: 自动记录触发

| 触发源 | 记录的决策 |
|--------|-----------|
| `add_contract_item(confidence=confirmed)` | Confirmed 类型：条目内容 + 选择理由 |
| `add_contract_item(confidence=inferred)` | Inferred 类型：Agent 推断的条目 |
| `update_contract_item` | Revised 类型：旧内容 → 新内容 + 修改原因 |
| 用户选择中未被选中的选项 | Rejected 类型：被否决方案的记录 |
| 用户选择"你决定"/"跳过" | Skipped 类型 |

**FR-10.6.2a**: 每个 `add_contract_item` 工具调用自动创建一条 DecisionEntry  
**FR-10.6.2b**: `present_choices` 的用户选择结果中，未被选中的选项记录为 alternatives  
**FR-10.6.2c**: `update_contract_item` 调用时，自动将旧内容记录为 Revised 类型的 DecisionEntry

### FR-10.6.3: 数据库持久化

```sql
-- 011_decision_log (第 11 次迁移)
CREATE TABLE IF NOT EXISTS decision_log (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES preflight_sessions(id) ON DELETE CASCADE,
    round INTEGER NOT NULL,
    decision_type TEXT NOT NULL CHECK (decision_type IN ('confirmed', 'rejected', 'inferred', 'revised', 'skipped')),
    description TEXT NOT NULL,
    rationale TEXT NOT NULL DEFAULT '',
    alternatives TEXT NOT NULL DEFAULT '[]',  -- JSON 数组
    contract_item_id TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX idx_decision_log_session ON decision_log(session_id);
```

### FR-10.6.4: 查询 API

- **FR-10.6.4a**: 新增 Tauri command `get_decision_log(session_id)` 返回该 session 的全部决策记录
- **FR-10.6.4b**: 支持按 `decision_type` 过滤
- **FR-10.6.4c**: 返回结果按 `round` 升序排列

### FR-10.6.5: 注入 System Prompt

决策日志的关键信息注入 LLM 的 system prompt（通过 FM-10.3 动态段）：

**FR-10.6.5a**: 被否决方案列表注入为 "已否决方案" 段：
```
# 已否决方案（请勿再建议）
- 自建认证系统 (第 3 轮否决，原因: 用户偏好 OAuth)
- 短期会话 (第 5 轮否决，原因: 用户需要长期登录态)
```

**FR-10.6.5b**: 最多注入最近 10 条被否决方案  
**FR-10.6.5c**: 此段的 token 上限为 300 tokens

### FR-10.6.6: 前端展示

- **FR-10.6.6a**: `ContractPanel` 底部新增"决策历程"折叠区域
- **FR-10.6.6b**: 展示决策时间线：每条记录显示轮次、类型图标、决策描述
- **FR-10.6.6c**: Confirmed 类型显示为绿色，Rejected 显示为红色，Revised 显示为橙色
- **FR-10.6.6d**: 点击 Contract 条目可跳转/高亮关联的决策记录

### FR-10.6.7: 签署前决策摘要

- **FR-10.6.7a**: 签署 Contract 时，弹窗中展示关键决策摘要（confirmed + revised 类型）
- **FR-10.6.7b**: 摘要按 Contract 区块分组展示
- **FR-10.6.7c**: 用户可点击展开查看每条决策的理由和被否决的替代方案

---

## 4. 非功能需求

| ID | 类型 | 要求 |
|----|------|------|
| NFR-10.6.1 | 性能 | 决策记录写入延迟 ≤ 2ms |
| NFR-10.6.2 | 存储 | 单条记录 ≤ 1KB |
| NFR-10.6.3 | 可扩展性 | 支持新增 DecisionType 不修改现有逻辑 |
| NFR-10.6.4 | 兼容性 | 旧 session（无 decision_log）正常加载，显示空时间线 |

---

## 5. 效果度量

### 5.1 定量指标

| 指标 | 度量方法 | 基线（当前） | 目标 | 采集方式 |
|------|----------|-------------|------|----------|
| **决策覆盖率** | `有决策记录的 contract_item / 所有 contract_item` | 0% | ≥ 90% | DB 查询：`LEFT JOIN decision_log ON contract_item_id` |
| **否决方案记录率** | `有 alternatives 的决策 / 来自 present_choices 的决策` | 0% | ≥ 80% | DB 查询 |
| **LLM 重复建议率** | `Agent 建议了已否决方案的轮次 / 总轮次` | ~15%（无否决记录，LLM 反复建议） | ≤ 3% | 人工标注 10 轮对话 |
| **System prompt 注入 token** | 被否决方案段的 token 数 | 0 | ≤ 300 tokens | 后端日志 |

### 5.2 定性验证

| 验证项 | 方法 | 通过标准 |
|--------|------|----------|
| **可追溯性** | 随机选取 3 个 Contract 条目，检查其关联的 DecisionEntry | 3/3 都有决策记录且理由清晰 |
| **否决方案不重复** | 否决某方案后继续对话 5 轮 | Agent 不再建议该方案 |
| **修改历史完整** | 修改 1 个 Contract 条目 | DecisionLog 中有 Revised 记录，含旧内容和修改原因 |
| **签署前回顾体验** | 签署时阅读决策摘要 | 摘要涵盖主要决策，用户可快速回顾 |

### 5.3 用户体验度量

| 维度 | 度量方式 | 通过标准 |
|------|----------|----------|
| **决策时间线可读性** | 展示 10 条决策记录的时间线给 3 位测试者 | ≥ 2 人认为"容易理解对话过程" |
| **条目→决策跳转** | 点击 Contract 条目 → 高亮决策记录 | 跳转 < 200ms，目标决策可见 |
| **签署摘要有用性** | 3 位测试者在签署前阅读摘要 | ≥ 2 人认为"帮助了我做出签署决定" |

---

## 6. 实现要点

### 6.1 后端改动

| 文件 | 改动 |
|------|------|
| `db/migrations.rs` | 新增第 11 次迁移：`decision_log` 表 |
| `agent/planner.rs` | 在处理 `add_contract_item`/`update_contract_item` tool_call 时自动写入 DecisionEntry |
| `agent/planner.rs` | 在处理 `present_choices` 的用户选择时，记录未选中选项为 alternatives |
| `agent/planner.rs` | `build_preflight_system_prompt()` 新增"已否决方案"段 |
| `commands/preflight.rs` | 新增 `get_decision_log` command |

### 6.2 前端改动

| 文件 | 改动 |
|------|------|
| `components/preflight/ContractPanel.tsx` | 新增底部"决策历程"折叠区域 |
| `components/preflight/DecisionTimeline.tsx` (新) | 决策时间线组件 |
| `components/preflight/DecisionTimeline.module.css` (新) | 时间线样式 |
| `ipc/commands.ts` | 新增 `getDecisionLog` IPC 封装 |

---

## 7. 风险

| 风险 | 概率 | 影响 | 缓解 |
|------|------|------|------|
| LLM 的 `add_contract_item.rationale` 质量不一致 | 中 | 低 | 工具 schema 的 rationale 设为 required 并在 description 中强调 |
| 决策记录过多导致 system prompt 超限 | 低 | 低 | 只注入被否决方案，且限制 10 条 + 300 tokens |
| 决策与 Contract 条目的关联可能断裂 | 低 | 低 | 使用外键约束，删除条目时级联处理 |
| 前端时间线 UI 在决策过多时性能下降 | 低 | 低 | 默认折叠，虚拟滚动 |
