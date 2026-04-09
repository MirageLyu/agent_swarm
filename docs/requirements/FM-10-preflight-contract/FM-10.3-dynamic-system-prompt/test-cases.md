# FM-10.3: Dynamic System Prompt Assembly — 测试用例

> 版本: v1.0 | 日期: 2026-04-09

---

## 单元测试 (UT)

### UT-10.3.1: Prompt 组装引擎（Rust）

| ID | 场景 | 输入 | 期望结果 |
|----|------|------|----------|
| UT-10.3.1a | 基础组装 | mode=scenario_walk, 空 Contract, 初始 BeliefState | 输出含静态前缀 + 动态后缀，用 `__DYNAMIC_BOUNDARY__` 分隔 |
| UT-10.3.1b | 含 Contract 条目 | 3 个 scope + 1 个 constraints 条目 | 动态后缀的 contract_state 含 4 条目 JSON |
| UT-10.3.1c | 含 Belief State | convergence_score=0.6, phase=Narrowing | 动态后缀含收敛分数和阶段 |
| UT-10.3.1d | 含收敛指令 | phase=ReadyToSign | 动态后缀含"调用 suggest_sign" |
| UT-10.3.1e | 模式切换 | mode=devils_advocate | 模式段内容为魔鬼代言人指引 |
| UT-10.3.1f | 总 token 限制 | 超大 Contract (30 条目) | 总 prompt ≤ 2000 tokens |

### UT-10.3.2: Contract 紧凑 JSON 生成（Rust）

| ID | 场景 | 输入 | 期望结果 |
|----|------|------|----------|
| UT-10.3.2a | 空 Contract | 无条目 | `{"scope":[],"constraints":[],"exclusions":[],"assumptions":[]}` |
| UT-10.3.2b | 正常 Contract | 5 条目 | 各条目包含 `text(confidence)` 格式 |
| UT-10.3.2c | 条目省略 | 25 条目 | 最近 5 轮新增的全文显示，其余省略为 `"...及另外 N 条"` |
| UT-10.3.2d | Token 上限 | 构造使 JSON 超 800 tokens 的 Contract | 自动截断/省略至 ≤ 800 tokens |

### UT-10.3.3: 各段独立性验证（Rust）

| ID | 场景 | 操作 | 期望结果 |
|----|------|------|----------|
| UT-10.3.3a | 禁用 contract_state 段 | 设置段为 disabled | Prompt 中无 contract_state，其他段正常 |
| UT-10.3.3b | 禁用 convergence_directive 段 | 设置段为 disabled | Prompt 中无收敛指令，其他段正常 |
| UT-10.3.3c | 修改模式段内容 | 更新 scenario_walk 指引文案 | 仅模式段变化，其余段不变 |

### UT-10.3.4: 静态前缀稳定性（Rust）

| ID | 场景 | 操作 | 期望结果 |
|----|------|------|----------|
| UT-10.3.4a | 连续组装 | 同一 session 连续调用 10 次，Contract/BeliefState 不同 | 静态前缀字节级一致（SHA-256 相同） |
| UT-10.3.4b | 不同模式 | mode=scenario_walk vs devils_advocate | 静态前缀相同（模式属于动态段） |
| UT-10.3.4c | 不同 session | 两个不同 session | 静态前缀相同 |

### UT-10.3.5: Belief State 渲染（Rust）

| ID | 场景 | 输入 | 期望输出包含 |
|----|------|------|-------------|
| UT-10.3.5a | 早期状态 | score=0.15, phase=Exploring, 8 unfilled slots | "探索阶段" + unfilled slot 名称列表 |
| UT-10.3.5b | 中期状态 | score=0.55, phase=Narrowing, 3 tentative + 2 unfilled | "收窄阶段" + tentative 和 unfilled 列表 |
| UT-10.3.5c | 就绪状态 | score=0.92, phase=ReadyToSign | "就绪" + 收敛分数 |
| UT-10.3.5d | Token 上限 | 全部 10 个 slot 有详细状态 | 渲染结果 ≤ 200 tokens |

### UT-10.3.6: 项目上下文注入（Rust）

| ID | 场景 | 输入 | 期望结果 |
|----|------|------|----------|
| UT-10.3.6a | 有项目描述 | project_desc="电商平台，React+Node" | 消息列表首条为 `<project-context>` user 消息 |
| UT-10.3.6b | 无项目描述 | 未配置 | 不注入额外消息 |
| UT-10.3.6c | 含"不一定相关"提示 | 有项目描述 | 消息文本包含"不一定与当前讨论直接相关" |

---

## 集成测试 (IT)

### IT-10.3.1: 动态 Prompt 端到端

**步骤**:
1. 启动 Pre-flight("实现用户认证系统")
2. 对话 3 轮，每轮确认一些选项
3. 检查第 4 轮的 LLM 请求中 system prompt 内容

**验证点**:
- [ ] System prompt 包含静态前缀（角色定义、工具规范）
- [ ] System prompt 包含 Contract 当前条目
- [ ] System prompt 包含收敛分数和阶段
- [ ] System prompt 包含当前模式指引
- [ ] Agent 不重复询问已确认的条目

### IT-10.3.2: 模式切换 Prompt 变化

**步骤**:
1. 启动 Pre-flight，默认 scenario_walk 模式
2. 对话 2 轮
3. 切换到 devils_advocate 模式
4. 对话 1 轮

**验证点**:
- [ ] 切换后 system prompt 的模式段变为魔鬼代言人指引
- [ ] 静态前缀未变化
- [ ] Agent 提问风格从"引导式"变为"质疑式"

### IT-10.3.3: Prompt Token 预算验证

**步骤**:
1. 启动 Pre-flight，对话 10 轮，Contract 累积 15+ 条目
2. 检查每轮 system prompt 的 token 数

**验证点**:
- [ ] 每轮 system prompt ≤ 2000 tokens
- [ ] Contract 条目自动省略机制在条目 > 20 时生效
- [ ] 后端日志记录每轮各段 token 数

---

## 行为测试 (BT)

### BT-10.3.1: 重复提问率对比

**目的**: 验证动态 prompt 显著降低 Agent 重复提问

**步骤**:
1. 用"实现用户认证系统"分别运行：
   - A组: 动态 prompt（含 Contract + BeliefState 注入）
   - B组: 静态 prompt（当前方案）
2. 各运行 3 次，每次 8 轮
3. 人工标注每轮是否在提问已确认的内容

**度量**:
| 指标 | 计算方式 | A组(动态)目标 | B组(静态)基线 |
|------|----------|-------------|-------------|
| 重复提问率 | 重复轮 / 总轮 | ≤ 5% | ~20% |
| 信息利用率 | Agent 引用 Contract 内容的轮次 / 总轮次 | ≥ 40% | ~10% |

### BT-10.3.2: Prompt 各段贡献度验证

**目的**: 验证每个动态段对 LLM 行为的实际影响

**步骤**:
1. 在第 5 轮分别测试以下配置：
   - 全功能（所有段启用）
   - 禁用 contract_state
   - 禁用 belief_state
   - 禁用 convergence_directive
2. 观察 LLM 输出差异

**度量**:
| 配置 | 期望行为差异 |
|------|-------------|
| 全功能 | Agent 感知当前状态，避免重复，按阶段调整策略 |
| 无 contract_state | Agent 可能重复已确认的问题 |
| 无 belief_state | Agent 不知道哪些领域未覆盖 |
| 无 convergence_directive | Agent 在 ReadyToSign 阶段仍持续提问 |

---

### BT-10.3.3: Extended Thinking 互斥验证

**目的**: 验证 Thinking API 开启时 prompt 中不含 CoT 引导

**步骤**:
1. 配置 `supports_thinking=true` 的模型
2. 构建 system prompt
3. 检查 prompt 内容

**度量**:
| 检查项 | 通过标准 |
|--------|----------|
| Prompt 不含 "think step by step" | 是 |
| Prompt 不含 "请先分析" | 是 |
| Prompt 不含 `<analysis>` 标签引导 | 是 |
| LLM 请求参数含 `thinking` 或 `enable_thinking` | 是 |

### BT-10.3.4: 模型切换适配验证

**目的**: 验证切换模型后 prompt 自动适配

**步骤**:
1. 用 qwen3.5-plus (无 thinking) 开始对话 3 轮
2. 切换到 qwen3 (有 thinking)
3. 继续对话

**度量**:
| 维度 | 通过标准 |
|------|----------|
| Prompt 自动移除 CoT 引导 | 是 |
| LLM 请求参数自动添加 thinking | 是 |
| 对话功能不中断 | 是 |

---

## 单元测试补充 — Model Capability Registry

### UT-10.3.7: ModelCapabilities 查询（Rust）

| ID | 场景 | 输入 | 期望结果 |
|----|------|------|----------|
| UT-10.3.7a | 内置模型查询 | provider=dashscope, model=qwen3.5-plus | supports_tool_use=true, supports_thinking=false |
| UT-10.3.7b | 带 thinking 的模型 | provider=dashscope, model=qwen3 | supports_thinking=true |
| UT-10.3.7c | 未知模型 | provider=unknown, model=unknown | 全部 false (安全默认值) |
| UT-10.3.7d | 用户自定义覆盖 | 用户配置 supports_thinking=true | 覆盖内置值 |

### UT-10.3.8: Thinking 互斥检查（Rust）

| ID | 场景 | 输入 | 期望结果 |
|----|------|------|----------|
| UT-10.3.8a | Thinking + 无 CoT | supports_thinking=true, prompt 无 CoT | 通过 |
| UT-10.3.8b | 无 Thinking + 有 CoT | supports_thinking=false, prompt 含 `<analysis>` | 通过 |
| UT-10.3.8c | Thinking + 有 CoT | supports_thinking=true, prompt 含 CoT | 断言失败 / warn 日志 |

### UT-10.3.9: extract_reasoning 统一接口（Rust）

| ID | 场景 | 输入 | 期望结果 |
|----|------|------|----------|
| UT-10.3.9a | 有 thinking block | supports_thinking=true, response 含 thinking block | 提取 thinking 文本 |
| UT-10.3.9b | 有 analysis 标签 | supports_thinking=false, text 含 `<analysis>推理</analysis>` | 提取 "推理" |
| UT-10.3.9c | 无推理内容 | 两者都无 | 返回 None |

---

## 回归测试 (RT)

| ID | 验证项 | 方法 |
|----|--------|------|
| RT-10.3.1 | 模式切换不丢失 Contract | 切换模式后 Contract 条目仍在 prompt 中 |
| RT-10.3.2 | 第 1 轮正常启动 | 空 Contract + 初始 BeliefState 时 prompt 格式正确 |
| RT-10.3.3 | Quick Plan 不受影响 | Quick Plan 使用原有 system prompt，不走新逻辑 |
| RT-10.3.4 | 未配置能力注册表 | 默认 ModelCapabilities → 功能正常（保守模式） |
