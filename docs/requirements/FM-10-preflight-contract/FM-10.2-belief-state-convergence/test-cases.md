# FM-10.2: Belief State & Convergence — 测试用例

> 版本: v1.0 | 日期: 2026-04-09

---

## 单元测试 (UT)

### UT-10.2.1: PreflightBeliefState 初始化（Rust）

| ID | 场景 | 输入 | 期望结果 |
|----|------|------|----------|
| UT-10.2.1a | 默认初始化 | 无参数 | 10 个预定义 slot 全部 Unfilled，convergence_score=0.0，phase=Exploring |
| UT-10.2.1b | 自定义 slot | 自定义 3 个 slot + 权重 | 仅含 3 个 slot，权重和为 1.0 |
| UT-10.2.1c | 自定义 max_rounds | `max_rounds=8` | 构造成功，max_rounds=8 |
| UT-10.2.1d | 权重和 ≠ 1.0 | 权重总和 = 0.9 | 自动归一化至 1.0 |

### UT-10.2.2: 收敛分数计算（Rust）

| ID | 场景 | Slot 状态分布 | 期望 convergence_score |
|----|------|--------------|----------------------|
| UT-10.2.2a | 全部 Unfilled | 10 × Unfilled | 0.0 |
| UT-10.2.2b | 全部 Confirmed | 10 × Confirmed | 1.0 |
| UT-10.2.2c | 混合状态 | primary_goal(0.2)=Confirmed + target_users(0.1)=Tentative + 其余 Unfilled | 0.2×1.0 + 0.1×0.5 = 0.25 |
| UT-10.2.2d | 含 Skipped | primary_goal(0.2)=Confirmed + out_of_scope(0.1)=Skipped + 其余 Unfilled | 0.2×1.0 + 0.1×0.8 = 0.28 |
| UT-10.2.2e | 高收敛 | 9 个 Confirmed + 1 个 Tentative(权重0.05) | ≈ 0.975 |

### UT-10.2.3: 阶段自动转移（Rust）

| ID | 场景 | 条件 | 期望 phase |
|----|------|------|-----------|
| UT-10.2.3a | 初始状态 | round=0, score=0.0 | Exploring |
| UT-10.2.3b | Score 驱动转移 | round=2, score=0.35 | Narrowing |
| UT-10.2.3c | Round 驱动转移 | round=3, score=0.15 | Narrowing |
| UT-10.2.3d | 进入 Confirming | round=5, score=0.70 | Confirming |
| UT-10.2.3e | 进入 ReadyToSign | round=8, score=0.90 | ReadyToSign |
| UT-10.2.3f | 高分数快速就绪 | round=4, score=0.90 | ReadyToSign (跳过 Confirming) |
| UT-10.2.3g | 阶段不回退 | 先到 Narrowing，score 短暂降到 0.25 | 保持 Narrowing（不回退到 Exploring） |

### UT-10.2.4: Slot 自动填充映射（Rust）

| ID | 场景 | add_contract_item 参数 | 期望 slot 更新 |
|----|------|----------------------|---------------|
| UT-10.2.4a | Scope → primary_goal | `section=scope, item="实现用户登录系统", confidence=confirmed` | `primary_goal` → Confirmed |
| UT-10.2.4b | Scope → target_users | `section=scope, item="面向B端企业用户", confidence=tentative` | `target_users` → Tentative |
| UT-10.2.4c | Constraints → tech | `section=constraints, item="使用React+Node.js", confidence=confirmed` | `tech_constraints` → Confirmed |
| UT-10.2.4d | Exclusions → out_of_scope | `section=exclusions, item="不包含支付功能", confidence=confirmed` | `out_of_scope` → Confirmed |
| UT-10.2.4e | 无匹配 | `section=scope, item="其他补充说明"` | 不更新任何预定义 slot（或更新通用 slot） |
| UT-10.2.4f | 重复填充同一 slot | 同一 slot 再次被 confirmed | slot 保持 Confirmed，不降级 |

### UT-10.2.5: 收敛指令生成（Rust）

| ID | 场景 | 输入 | 期望指令内容 |
|----|------|------|-------------|
| UT-10.2.5a | Exploring 阶段 | phase=Exploring, unfilled=[target_users, tech_constraints, ...] | 包含"探索阶段"和未触及 slot 列表 |
| UT-10.2.5b | Narrowing 阶段 | phase=Narrowing, tentative=[key_features], unfilled=[security] | 包含"收窄阶段"和待确认 slot |
| UT-10.2.5c | Confirming 阶段 | phase=Confirming, score=0.78 | 包含"确认阶段"和收敛分数 |
| UT-10.2.5d | ReadyToSign | phase=ReadyToSign, score=0.92 | 包含"调用 suggest_sign" |

### UT-10.2.6: 数据库持久化（Rust）

| ID | 场景 | 操作 | 期望结果 |
|----|------|------|----------|
| UT-10.2.6a | 存储 belief_state | 更新 preflight_sessions | belief_state JSON 可 roundtrip |
| UT-10.2.6b | 读取旧 session | 无 belief_state 列的旧记录 | 自动初始化默认 BeliefState |
| UT-10.2.6c | 读取 phase | DB 中 phase="narrowing" | 解析为 ConversationPhase::Narrowing |

---

## 集成测试 (IT)

### IT-10.2.1: Belief State 全链路更新

**步骤**:
1. `start_preflight("实现用户认证系统")`
2. 进行 3 轮对话
3. 每轮后查询 belief_state

**验证点**:
- [ ] 每轮 `convergence_score` 单调递增（或不减）
- [ ] 至少 2 个 slot 从 Unfilled 变为 Tentative/Confirmed
- [ ] phase 从 Exploring 变为 Narrowing
- [ ] 前端 `PreflightStatusBar` 同步更新

### IT-10.2.2: 收敛驱动签署建议

**步骤**:
1. 启动 Pre-flight，充分对话直到 `convergence_score ≥ 0.85`
2. 观察 LLM 是否调用 `suggest_sign`

**验证点**:
- [ ] phase 变为 ReadyToSign
- [ ] LLM 在 1-2 轮内调用 `suggest_sign`（依赖 FM-10.1 工具）
- [ ] 前端进度条显示"就绪"状态
- [ ] 签署建议包含 `readiness_assessment` 数据

### IT-10.2.3: 简单需求快速路径

**步骤**:
1. `start_preflight("给项目添加 README 文件")`
2. 每轮选择 Agent 推荐的选项

**验证点**:
- [ ] ≤ 4 轮到达 ReadyToSign
- [ ] `convergence_score ≥ 0.85`
- [ ] 未填充的 slot 均为低权重或被 Skipped

---

## 行为测试 (BT)

### BT-10.2.1: 收敛曲线合理性

**目的**: 验证 convergence_score 的变化曲线符合直觉

**步骤**:
1. 用"实现用户认证系统"运行 10 轮 Pre-flight
2. 记录每轮的 `convergence_score` 和 `phase`

**度量**:
| 指标 | 计算方式 | 通过标准 |
|------|----------|----------|
| 单调性 | score 非递减的轮次占比 | ≥ 80% (允许小幅波动) |
| 阶段覆盖 | 对话中出现的不同阶段数 | ≥ 3 (Exploring → Narrowing → Confirming+) |
| 合理终止 | ReadyToSign 时的 score | ≥ 0.80 |

### BT-10.2.2: 不同复杂度需求的收敛差异

**目的**: 验证简单需求快速收敛，复杂需求充分展开

**步骤**:
1. 简单需求: "添加 .gitignore 文件" → 记录收敛轮次
2. 中等需求: "实现用户认证系统" → 记录收敛轮次
3. 复杂需求: "多租户 SaaS 电商平台" → 记录收敛轮次

**度量**:
| 需求类型 | 期望收敛轮次 | 期望 Contract items |
|----------|------------|-------------------|
| 简单 | ≤ 4 | 4-8 |
| 中等 | 5-8 | 8-15 |
| 复杂 | 8-12 | 15+ |

### BT-10.2.3: 前端进度条体验

**目的**: 验证 convergence_score 驱动的进度条比旧方案更平滑

**步骤**:
1. 录制 10 轮对话中进度条的截图序列
2. 与旧方案（5 档跳跃）对比

**度量**:
| 维度 | 旧方案 | 新方案目标 |
|------|--------|-----------|
| 进度档位数 | 5 (0/20/40/60/80/100) | ≥ 8 个不同取值 |
| 最大单次跳跃 | 20% | ≤ 15% |
| 进度条与体感偏差 | "100%但还在问" | 进度条准确反映可签署程度 |

---

## 回归测试 (RT)

| ID | 验证项 | 方法 |
|----|--------|------|
| RT-10.2.1 | 无 belief_state 的旧 session 可加载 | 清除 belief_state 列 → 加载 session → 不报错 |
| RT-10.2.2 | Contract 手动编辑不破坏 belief_state | 手动添加 contract_item → belief_state 更新 |
| RT-10.2.3 | Quick Plan 不受影响 | Quick Plan 流程无 belief_state 参与 |
