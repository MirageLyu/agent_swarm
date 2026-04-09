# FM-10.2: Belief State & Convergence — 信念状态与收敛机制

> 版本: v1.0 | 日期: 2026-04-09  
> 优先级: **P0 (Belief State) + P1 (Convergence)** | 预估周期: 2 天  
> 依赖: FM-10.1 (Tool-as-Structure) | 被依赖: FM-10.3, FM-10.5  
> 调研来源: Claude Code 架构分析 §模式7; Stanford SLP3; ACM Survey 2025 (doi:10.1145/3771090)

---

## 1. 目标

引入显式的 **Belief State** 数据结构和程序化的 **收敛机制**，解决当前 Pre-flight 的两大问题：

1. **无法量化澄清进度**：进度条基于轮次计数，与实际 Contract 完成度脱节
2. **不知何时停止**：依赖硬编码的轮次阈值（5 轮/8 轮），LLM 无结构化信号判断是否应停止提问

---

## 2. 现状分析

| 维度 | 当前实现 | 问题 |
|------|----------|------|
| 进度追踪 | 前端 `computeProgress()` 基于 Contract 四区块"是否有至少 1 条目" | 粒度太粗，只有 0/20/40/60/80/100 五档 |
| 收敛判断 | `planner.rs` 中根据 `user_rounds >= 5/8` 硬编码收敛指令 | 与需求复杂度无关，简单需求也要 5 轮 |
| 对话状态 | 隐含在消息历史中，LLM 需自行"读懂"过去的对话 | 信息随轮次增多而"稀释"，LLM 注意力下降 |
| 阶段感知 | 无 | LLM 不知道自己在"探索"还是"确认"阶段 |

---

## 3. 功能需求

### FR-10.2.1: PreflightBeliefState 数据结构

定义结构化的信念状态，包含以下维度：

```rust
struct PreflightBeliefState {
    round: u32,
    max_rounds: u32,  // 默认 12，可配置
    
    // 语义槽位 (slots)
    slots: HashMap<String, SlotState>,
    
    // 收敛分数 (0.0 - 1.0)
    convergence_score: f64,
    
    // 对话阶段
    phase: ConversationPhase,
}

enum SlotStatus {
    Unfilled,
    Tentative,   // LLM 推断但用户未确认
    Confirmed,   // 用户明确确认
    Skipped,     // 用户明确跳过
}

enum ConversationPhase {
    Exploring,     // 广度探索，收集高层需求
    Narrowing,     // 逐步收窄，处理细节
    Confirming,    // 确认已收集信息
    ReadyToSign,   // 建议签署
}
```

### FR-10.2.2: 预定义语义槽位

系统提供默认槽位集合，覆盖常见软件需求维度：

| Slot ID | 名称 | 权重 | 所属区块 |
|---------|------|------|----------|
| `primary_goal` | 核心目标 | 0.20 | scope |
| `target_users` | 目标用户 | 0.10 | scope |
| `key_features` | 关键功能列表 | 0.15 | scope |
| `tech_constraints` | 技术约束 | 0.10 | constraints |
| `performance_targets` | 性能目标 | 0.05 | constraints |
| `security_requirements` | 安全需求 | 0.08 | constraints |
| `integration_points` | 集成点 | 0.07 | scope |
| `out_of_scope` | 明确排除项 | 0.10 | exclusions |
| `risk_assumptions` | 风险与假设 | 0.08 | assumptions |
| `timeline_budget` | 时间/预算约束 | 0.07 | constraints |

**FR-10.2.2a**: 槽位集合可在 `contract_config` 中自定义（增/删/改权重）  
**FR-10.2.2b**: 权重之和必须为 1.0，用于计算加权收敛分数

### FR-10.2.3: 收敛分数计算

```
convergence_score = Σ (slot_weight × slot_score)

slot_score:
  Unfilled  → 0.0
  Tentative → 0.5
  Confirmed → 1.0
  Skipped   → 0.8 (明确跳过也是一种确认)
```

**FR-10.2.3a**: 每轮对话后后端自动重新计算 `convergence_score`  
**FR-10.2.3b**: 计算结果持久化到 `preflight_sessions` 表（新增 `belief_state` JSON 列）  
**FR-10.2.3c**: 计算结果通过 `preflight-stream` 事件推送到前端

### FR-10.2.4: 阶段自动转移

| 当前阶段 | 转移条件 | 目标阶段 |
|----------|----------|----------|
| Exploring | `convergence_score ≥ 0.3` 或 `round ≥ 3` | Narrowing |
| Narrowing | `convergence_score ≥ 0.65` 或 `round ≥ 7` | Confirming |
| Confirming | `convergence_score ≥ 0.85` 或 `round ≥ 10` | ReadyToSign |
| ReadyToSign | 用户签署 Contract | (结束) |

**FR-10.2.4a**: 阶段转移由后端在每轮 `preflight_chat` 结束后自动判断  
**FR-10.2.4b**: 阶段变化时通过 `preflight-stream` 事件通知前端  
**FR-10.2.4c**: 阶段变化记录到决策日志（FM-10.6）

### FR-10.2.5: 收敛指令生成

根据当前 Belief State 生成动态收敛指令，注入 system prompt（具体注入见 FM-10.3）：

| 阶段 | 收敛指令内容 |
|------|-------------|
| Exploring | `"当前处于探索阶段。请广泛了解用户需求，覆盖尽量多的 slot。\n未触及的关键领域: {unfilled_slots}"` |
| Narrowing | `"当前处于收窄阶段。请针对未确认的 slot 深入提问。\n待确认: {tentative_slots}\n未触及: {unfilled_slots}"` |
| Confirming | `"当前处于确认阶段。请复述已确认的决策，确认用户无异议。\n收敛分数: {score}%"` |
| ReadyToSign | `"澄清已充分（{score}%）。请调用 suggest_sign 工具建议签署。"` |

### FR-10.2.6: Slot 自动填充

- **FR-10.2.6a**: 当 `add_contract_item` 工具被调用时，后端根据 `section` 和 `item` 内容自动匹配最相关的 slot 并更新其状态
- **FR-10.2.6b**: 匹配逻辑优先使用精确映射（`section=scope` + 关键词包含 "目标/功能/用户" → 对应 slot），无匹配时标记为通用 slot
- **FR-10.2.6c**: `confidence=confirmed` 时 slot 状态设为 Confirmed，`confidence=tentative/inferred` 时设为 Tentative

### FR-10.2.7: 前端进度展示

- **FR-10.2.7a**: `PreflightStatusBar` 使用 `convergence_score` 替代当前的区块存在性检查
- **FR-10.2.7b**: 进度条旁显示当前阶段标签（探索 / 收窄 / 确认 / 就绪）
- **FR-10.2.7c**: 进度条颜色随阶段变化：探索(蓝) → 收窄(紫) → 确认(绿) → 就绪(金)

---

## 4. 非功能需求

| ID | 类型 | 要求 |
|----|------|------|
| NFR-10.2.1 | 性能 | Belief State 计算延迟 ≤ 5ms（纯内存计算） |
| NFR-10.2.2 | 存储 | Belief State JSON 序列化后 ≤ 2KB |
| NFR-10.2.3 | 可扩展性 | 支持运行时动态添加/删除 slot，无需修改代码 |
| NFR-10.2.4 | 兼容性 | 已存在的 preflight session（无 belief_state）可正常加载，自动初始化默认状态 |

---

## 5. 效果度量

### 5.1 定量指标

| 指标 | 度量方法 | 基线（当前） | 目标 | 采集方式 |
|------|----------|-------------|------|----------|
| **平均收敛轮次** | 从 Pre-flight 开始到 `convergence_score ≥ 0.85` 的平均轮次 | 不可量化（无收敛指标） | 简单需求 ≤ 5 轮，中等需求 ≤ 8 轮 | 后端日志：记录每轮 `convergence_score` |
| **收敛稳定性** | `score ≥ 0.85 后又回落 < 0.8 的概率` | N/A | ≤ 10% | 后端日志：监控 score 变化曲线 |
| **进度准确性** | `用户感知的完成度` vs `convergence_score` 的偏差 | 前端仅 5 档，偏差大 | 连续 0-100%，体感偏差 ≤ ±15% | 人工评测：5 位测试者评分 |
| **阶段转移合理性** | `阶段转移时机与人工标注的偏差轮次` | N/A（无阶段概念） | 偏差 ≤ 1 轮 | 人工评测：标注 15 轮对话的理想阶段 |
| **过度提问率** | `达到 ReadyToSign 后仍继续新问题的轮次 / 达到 ReadyToSign 后的总轮次` | ~40%（当前进度 100% 后仍继续提问） | ≤ 10% | 后端日志 |

### 5.2 定性验证

| 验证项 | 方法 | 通过标准 |
|--------|------|----------|
| **简单需求快速收敛** | "给项目添加 README" → Pre-flight | ≤ 4 轮到达 ReadyToSign |
| **复杂需求充分覆盖** | "多租户 SaaS 电商平台" → Pre-flight | ≥ 6 个 slot 被 Confirmed |
| **Skipped slot 处理** | 用户对某 slot 回复"你决定"/"跳过" | 对应 slot 变为 Skipped(0.8)，不影响收敛 |
| **阶段标签体感** | 5 位测试者观察阶段标签 | ≥ 4 人认为阶段标签准确反映当前对话状态 |

### 5.3 对比度量

使用"实现用户认证系统"需求，对比优化前后：

| 维度 | 优化前 | 优化后目标 | 度量方式 |
|------|--------|-----------|----------|
| 进度条到 100% 后仍提问的轮次 | 2-4 轮 | 0-1 轮 | 人工观察 |
| 进度条从 0 到 100% 的平滑性 | 5 档跳跃 | 连续递增 | 截图对比 |
| 用户对"何时可签署"的困惑 | 不确定何时签署 | 阶段标签给出清晰指引 | 用户反馈 |

---

## 6. 数据库变更

### 6.1 Schema 变更

```sql
-- 010_belief_state (第 10 次迁移)
ALTER TABLE preflight_sessions ADD COLUMN belief_state TEXT NOT NULL DEFAULT '{}';
ALTER TABLE preflight_sessions ADD COLUMN convergence_score REAL NOT NULL DEFAULT 0.0;
ALTER TABLE preflight_sessions ADD COLUMN phase TEXT NOT NULL DEFAULT 'exploring'
  CHECK (phase IN ('exploring', 'narrowing', 'confirming', 'ready_to_sign'));
```

---

## 7. 风险

| 风险 | 概率 | 影响 | 缓解 |
|------|------|------|------|
| Slot-Item 自动匹配准确性不足 | 中 | 中 | 先用关键词匹配，后续可引入 LLM 辅助分类 |
| 预定义 slot 不适用所有项目类型 | 中 | 低 | 支持自定义 slot 集合 |
| 收敛阈值需调优 | 高 | 中 | 阈值可配置，初期提供保守默认值 |
| Belief State 注入增加 system prompt 长度 | 确定 | 低 | 紧凑 JSON 格式，目标 ≤ 500 tokens |
