# FM-10.5: Context Compression — 测试用例

> 版本: v1.0 | 日期: 2026-04-09

---

## 单元测试 (UT)

### UT-10.5.1: Micro-compact（Rust）

| ID | 场景 | 输入 | 期望结果 |
|----|------|------|----------|
| UT-10.5.1a | 近期消息不压缩 | 10 条消息，当前轮=10，keep_recent=3 | 第 8-10 轮的 tool_result 完整保留 |
| UT-10.5.1b | 远期 present_choices 压缩 | 第 3 轮的 present_choices tool_result | 内容替换为 `[选项详情已压缩] 问题: "..." | 用户选择: "..."` |
| UT-10.5.1c | add_contract_item 不压缩 | 第 2 轮的 add_contract_item tool_result | 内容保持原样 |
| UT-10.5.1d | tool_use 记录保留 | 第 2 轮的 present_choices tool_use | 调用记录（函数名、参数）完整保留 |
| UT-10.5.1e | 空消息列表 | [] | 返回 [] |
| UT-10.5.1f | 无 tool 消息 | 仅 user + assistant 消息 | 消息列表不变 |
| UT-10.5.1g | 多个 tool_result 混合 | 含 present_choices + add_contract_item + suggest_sign | 仅 present_choices 被压缩 |

### UT-10.5.2: 元对话清理（Rust）

| ID | 场景 | 输入 | 期望结果 |
|----|------|------|----------|
| UT-10.5.2a | 问候语清理 | `"好的，让我们继续讨论。\n\n## 认证方式选择\n..."` (距当前 > 5 轮) | 问候语前缀被删除，保留核心内容 |
| UT-10.5.2b | 近期消息不清理 | 同上内容但距当前 2 轮 | 内容保持原样 |
| UT-10.5.2c | 纯分析内容 | `"## 认证方式分析\n..."` | 不做任何清理 |
| UT-10.5.2d | 无匹配模式 | 不含问候语/过渡语的文本 | 不做任何清理 |

### UT-10.5.3: Token 估算（Rust）

| ID | 场景 | 输入 | 期望结果 |
|----|------|------|----------|
| UT-10.5.3a | 短对话 | 3 轮对话消息 | 估算值在实际 token 数 ±20% 范围内 |
| UT-10.5.3b | 长对话 | 15 轮对话消息 | 估算值在实际 token 数 ±20% 范围内 |
| UT-10.5.3c | 含中文 | 中文为主的消息 | 估算准确（中文字符 ~1.5-2 tokens） |

### UT-10.5.4: Compaction 触发判断（Rust）

| ID | 场景 | 条件 | 期望 should_compact |
|----|------|------|-------------------|
| UT-10.5.4a | 实际 token 未达阈值 | last_input_tokens = 40% context window | false |
| UT-10.5.4b | 实际 token 达阈值 | last_input_tokens = 75% context window | true |
| UT-10.5.4c | 轮次达阈值 | round = 12, token = 50% | true |
| UT-10.5.4d | 已在 compact 模式 | querySource = "compact" | false (防死循环) |
| UT-10.5.4e | 熔断状态 | 连续失败 3 次 | false |
| UT-10.5.4f | 无 usage 数据退化 | last_input_tokens = None | 使用字符估算判断 |
| UT-10.5.4g | 预警阈值 | last_input_tokens = 58% context window | should_compact=false，但返回 warning=true |

### UT-10.5.4.1: Token 预算追踪（Rust）

| ID | 场景 | 输入 | 期望结果 |
|----|------|------|----------|
| UT-10.5.4.1a | 正常记录 | API 响应 usage={input: 2000, output: 500} | 持久化到 session，cumulative 累加 |
| UT-10.5.4.1b | 无 usage | API 响应无 usage 字段 | 不更新 token 字段，不报错 |
| UT-10.5.4.1c | 累计计算 | 3 轮后查询 | cumulative = 三轮 input_tokens 之和 |

### UT-10.5.5: 压缩后消息列表构建（Rust）

| ID | 场景 | 输入 | 期望结果 |
|----|------|------|----------|
| UT-10.5.5a | 正常构建 | 摘要文本 + 最近 3 轮消息 | [摘要 user 消息, 原始需求消息, 最近 3 轮消息] |
| UT-10.5.5b | 摘要前缀 | 摘要 user 消息 | 以"本次对话是对之前澄清的延续..."开头 |
| UT-10.5.5c | 原始需求保留 | 15 轮对话后压缩 | 用户最初的需求描述消息仍然存在 |
| UT-10.5.5d | 最近 3 轮完整性 | 最近 3 轮含 tool_use/tool_result | 完整保留，不被 micro-compact |

### UT-10.5.6: Fallback 机制（Rust）

| ID | 场景 | 条件 | 期望结果 |
|----|------|------|----------|
| UT-10.5.6a | 压缩超时 | LLM 摘要生成 > 30s | 退化为截断策略（删除前 50%） |
| UT-10.5.6b | 摘要为空 | LLM 返回空文本 | 退化为截断策略 |
| UT-10.5.6c | 连续 3 次失败 | 3 次 compaction 均失败 | 熔断，后续对话不再尝试压缩 |
| UT-10.5.6d | 截断策略正确性 | 10 轮消息截断 | 保留后 5 轮 + 原始需求消息 |

---

## 集成测试 (IT)

### IT-10.5.1: Micro-compact 端到端

**步骤**:
1. 启动 Pre-flight，对话 6 轮
2. 在第 7 轮检查发送给 LLM 的消息列表

**验证点**:
- [ ] 第 1-3 轮的 `present_choices` tool_result 被压缩
- [ ] 第 4-6 轮的 `present_choices` tool_result 保持完整
- [ ] 所有 `add_contract_item` tool_result 保持完整
- [ ] Agent 对话质量无明显下降

### IT-10.5.2: Full Compaction 端到端

**步骤**:
1. 启动 Pre-flight，持续对话 12 轮
2. 观察第 12 轮是否触发 compaction

**验证点**:
- [ ] 后端日志记录 compaction 触发
- [ ] 摘要包含关键决策和待澄清问题
- [ ] 第 13 轮 Agent 仍能引用之前的核心决策
- [ ] DB 中 `compacted_at` 和 `compaction_summary` 被填充

### IT-10.5.3: DB 原始数据完整性

**步骤**:
1. 对话 10 轮（触发 micro-compact）
2. 直接查询 `preflight_sessions.messages`

**验证点**:
- [ ] DB 中存储的是完整的原始消息历史
- [ ] micro-compact 仅作用于发给 LLM 的副本
- [ ] 可以从 DB 重建完整对话历史

---

## 行为测试 (BT)

### BT-10.5.1: Micro-compact Token 节省

**目的**: 验证 micro-compact 实际 token 节省效果

**步骤**:
1. 用"实现用户认证系统"进行 8 轮 Pre-flight
2. 记录每轮 micro-compact 前后的消息 token 数

**度量**:
| 轮次 | 压缩前 tokens | 压缩后 tokens | 节省率 |
|------|-------------|-------------|--------|
| 1-3 | 基线 | 无变化 | 0% (keep_recent=3) |
| 4 | T4 | T4' | (T4-T4')/T4 |
| ... | ... | ... | ... |
| 8 | T8 | T8' | 目标: ≥ 25% |

**通过标准**: 第 8 轮的节省率 ≥ 25%

### BT-10.5.2: Full Compaction 信息保留

**目的**: 验证压缩后 Agent 仍"记住"关键决策

**步骤**:
1. 对话 12 轮，建立包含 10+ 条目的 Contract
2. 触发 compaction
3. 向 Agent 提问 5 个关于之前决策的问题

**度量**:
| 检测问题 | 通过标准 |
|----------|----------|
| "我们确定的认证方式是什么？" | Agent 正确回答 |
| "有哪些功能被排除了？" | Agent 至少列出 1 个正确的排除项 |
| "为什么选择了 OAuth 而不是自建认证？" | Agent 能提到理由 |
| "当前还有什么没确认的？" | Agent 列出与 Belief State 匹配的 unfilled slots |
| "我之前否决了哪些方案？" | Agent 至少提到 1 个被否决的方案 |

**通过标准**: 5 个问题中 ≥ 4 个回答正确

### BT-10.5.3: 30 轮压力测试

**目的**: 验证系统在超长对话中的稳定性

**步骤**:
1. 用复杂需求"多租户 SaaS 电商平台"持续对话 30 轮
2. 预期触发 2-3 次 full compaction

**度量**:
| 指标 | 通过标准 |
|------|----------|
| 无报错完成 30 轮 | 是 |
| Agent 最后一轮仍能给出合理回复 | 回复与当前 Contract 一致 |
| 总 token 消耗 vs 无压缩理论值 | ≤ 无压缩值的 50% |
| Contract 条目数量 | ≥ 20 条 |

---

## 回归测试 (RT)

| ID | 验证项 | 方法 |
|----|--------|------|
| RT-10.5.1 | 短对话 (≤ 5 轮) 不受影响 | 3 轮对话 → 无 micro-compact 和 compaction 触发 |
| RT-10.5.2 | Quick Plan 不受影响 | Quick Plan 不涉及消息压缩 |
| RT-10.5.3 | 压缩后 Contract Panel 正常 | Compaction 不影响 Contract 条目显示 |
| RT-10.5.4 | 压缩后模式切换正常 | Compaction 后切换澄清模式 → 功能正常 |
