# FM-10.6: Decision Log — 测试用例

> 版本: v1.0 | 日期: 2026-04-09

---

## 单元测试 (UT)

### UT-10.6.1: DecisionEntry 创建（Rust）

| ID | 场景 | 输入 | 期望结果 |
|----|------|------|----------|
| UT-10.6.1a | Confirmed 决策 | `add_contract_item(section=scope, item="实现OAuth", confidence=confirmed, rationale="用户选择")` | 创建 DecisionEntry: type=Confirmed, contract_item_id 关联 |
| UT-10.6.1b | Inferred 决策 | `add_contract_item(confidence=inferred)` | 创建 DecisionEntry: type=Inferred |
| UT-10.6.1c | Revised 决策 | `update_contract_item(item_id, new_content, reason)` | 创建 DecisionEntry: type=Revised, description 含旧→新变更 |
| UT-10.6.1d | Skipped 决策 | 用户选择"你决定" | 创建 DecisionEntry: type=Skipped |
| UT-10.6.1e | 含 Alternatives | 用户从 A/B/C 中选择 A | DecisionEntry.alternatives 包含 B 和 C |

### UT-10.6.2: 数据库 CRUD（Rust）

| ID | 场景 | 操作 | 期望结果 |
|----|------|------|----------|
| UT-10.6.2a | 写入 | 创建 DecisionEntry | DB 插入成功，可查询 |
| UT-10.6.2b | 按 session 查询 | `get_decision_log(session_id)` | 返回该 session 的所有记录，按 round 升序 |
| UT-10.6.2c | 按 type 过滤 | `get_decision_log(session_id, type=rejected)` | 仅返回 Rejected 类型 |
| UT-10.6.2d | 级联删除 | 删除 session | 关联的 decision_log 记录被删除 |
| UT-10.6.2e | Alternatives 序列化 | 含 2 个替代方案 | JSON 数组 roundtrip 正确 |

### UT-10.6.3: 被否决方案注入（Rust）

| ID | 场景 | 输入 | 期望输出 |
|----|------|------|----------|
| UT-10.6.3a | 无被否决方案 | 空 decision_log | 不注入"已否决方案"段 |
| UT-10.6.3b | 3 个被否决方案 | 3 条 Rejected 记录 | 注入 3 行否决方案列表 |
| UT-10.6.3c | 超过 10 条 | 15 条 Rejected 记录 | 只注入最近 10 条 |
| UT-10.6.3d | Token 上限 | 大量长文本否决方案 | 注入段 ≤ 300 tokens |
| UT-10.6.3e | 格式正确 | 3 条记录 | 每行格式: `- {方案} (第{轮次}轮否决，原因: {原因})` |

### UT-10.6.4: 签署前摘要生成（Rust）

| ID | 场景 | 输入 | 期望结果 |
|----|------|------|----------|
| UT-10.6.4a | 有决策记录 | 8 条 Confirmed + 2 条 Revised | 摘要按 section 分组，含决策描述和理由 |
| UT-10.6.4b | 无决策记录 | 空 decision_log | 摘要显示"无结构化决策记录" |
| UT-10.6.4c | 含变更历史 | 1 条 Revised | 摘要中显示 "原: ... → 改为: ..." |

---

## 集成测试 (IT)

### IT-10.6.1: 自动记录完整链路

**步骤**:
1. `start_preflight("实现用户认证系统")`
2. 对话 4 轮，每轮选择一个选项
3. 查询 `get_decision_log(session_id)`

**验证点**:
- [ ] 至少 3 条 DecisionEntry（每轮选择产生 1 条 Confirmed）
- [ ] 每条记录的 `round` 对应实际轮次
- [ ] `contract_item_id` 关联正确的 Contract 条目
- [ ] 未选中的选项记录为 alternatives

### IT-10.6.2: 否决方案不重复建议

**步骤**:
1. 对话中明确否决某方案（如选择 OAuth 而非自建认证）
2. 继续对话 3 轮

**验证点**:
- [ ] Agent 不再建议"自建认证系统"
- [ ] system prompt 中包含被否决方案的记录
- [ ] 后端日志确认"已否决方案"段已注入

### IT-10.6.3: 条目修改追溯

**步骤**:
1. 通过 `add_contract_item` 添加一条 scope 条目
2. 通过 `update_contract_item` 修改该条目

**验证点**:
- [ ] decision_log 中有 2 条记录：1 条 Confirmed + 1 条 Revised
- [ ] Revised 记录包含旧内容和修改原因
- [ ] 前端时间线显示变更历史

### IT-10.6.4: 签署前摘要展示

**步骤**:
1. 对话 6 轮，积累多条决策
2. 点击签署 Contract

**验证点**:
- [ ] 签署弹窗展示决策摘要
- [ ] 摘要按 Contract 区块分组
- [ ] 每条决策显示理由
- [ ] 可展开查看被否决的替代方案

---

## 行为测试 (BT)

### BT-10.6.1: 决策覆盖率

**目的**: 验证自动记录机制的覆盖率

**步骤**:
1. 用"实现用户认证系统"完成 8 轮 Pre-flight
2. 对比 Contract 条目与 DecisionLog 的关联

**度量**:
| 指标 | 计算方式 | 通过标准 |
|------|----------|----------|
| 条目关联率 | `有决策记录的 contract_item / 全部 contract_item` | ≥ 90% |
| 否决记录率 | `有 alternatives 的 Confirmed 决策 / 全部 Confirmed 决策` | ≥ 70% |
| 理由填充率 | `rationale 非空的 DecisionEntry / 全部 DecisionEntry` | ≥ 80% |

### BT-10.6.2: 重复建议率对比

**目的**: 验证否决方案注入 prompt 后 LLM 不再重复建议

**步骤**:
1. A组（有决策日志注入）: 否决方案 X 后继续 5 轮
2. B组（无决策日志注入）: 否决方案 X 后继续 5 轮
3. 各运行 3 次

**度量**:
| 指标 | A组(有注入) | B组(无注入) |
|------|-----------|-----------|
| Agent 重新建议方案 X 的轮次 | ≤ 0.3 次/3次实验 | ≥ 1 次/3次实验 |

### BT-10.6.3: 前端时间线体验

**目的**: 验证决策时间线 UI 的可用性

**步骤**:
1. 完成 8 轮对话（产生 ~8-12 条决策记录）
2. 打开"决策历程"折叠区域

**度量**:
| 维度 | 通过标准 |
|------|----------|
| 渲染性能 | 展开时间线 < 100ms |
| 类型图标区分 | Confirmed(绿)/Rejected(红)/Revised(橙)  颜色正确 |
| 条目→决策关联 | 点击 Contract 条目 → 对应决策高亮 |
| 折叠/展开 | 默认折叠，点击展开/收起流畅 |

---

## 回归测试 (RT)

| ID | 验证项 | 方法 |
|----|--------|------|
| RT-10.6.1 | 旧 session 无 decision_log | 加载旧 session → 时间线显示"暂无记录" |
| RT-10.6.2 | Quick Plan 不受影响 | Quick Plan 不涉及 decision_log |
| RT-10.6.3 | Contract 手动编辑 | 手动添加/删除条目 → 不产生自动决策记录（仅 tool_use 触发） |
| RT-10.6.4 | 签署后 decision_log 只读 | Contract signed 后 → decision_log 不再写入 |
