# FM-10.4: Prompt Caching — 测试用例

> 版本: v1.0 | 日期: 2026-04-09

---

## 单元测试 (UT)

### UT-10.4.1: 缓存标记注入（Rust）

| ID | 场景 | 输入 | 期望结果 |
|----|------|------|----------|
| UT-10.4.1a | System prompt 标记 | 含 system 消息的 messages 列表 | system 消息的 `cache_control` = `{ type: "ephemeral" }` |
| UT-10.4.1b | 工具定义标记 | 5 个工具定义 | 最后一个工具的 `cache_control` = `{ type: "ephemeral" }` |
| UT-10.4.1c | 最后 user 消息标记 | 含 3 条 user 消息 | 仅最后一条 user 消息有 `cache_control` |
| UT-10.4.1d | 标记总数 | 完整的 messages + tools | `cache_control` 标记总数 ≤ 4 |
| UT-10.4.1e | 无 system 消息 | 仅含 user/assistant 消息 | 不报错，仅标记 user + tools |
| UT-10.4.1f | 空工具列表 | tools = [] | 不报错，仅标记 messages |

### UT-10.4.2: 请求序列化（Rust）

| ID | 场景 | 操作 | 期望结果 |
|----|------|------|----------|
| UT-10.4.2a | 含 cache_control 的序列化 | 序列化标记了缓存的请求 | JSON 输出含 `"cache_control": {"type": "ephemeral"}` |
| UT-10.4.2b | 无 cache_control 的序列化 | 序列化未标记的请求 | JSON 输出无 `cache_control` 字段 |
| UT-10.4.2c | 与 DashScope API 格式兼容 | 对比 DashScope 文档示例 | 格式一致 |

### UT-10.4.3: Usage 解析（Rust）

| ID | 场景 | API 响应 usage | 期望结果 |
|----|------|---------------|----------|
| UT-10.4.3a | 含缓存指标 | `{"input_tokens": 2000, "cache_read_input_tokens": 1500, "cache_creation_input_tokens": 0}` | 解析成功，hit_ratio = 0.75 |
| UT-10.4.3b | 无缓存指标 | `{"input_tokens": 2000}` | 缓存字段默认为 0，不报错 |
| UT-10.4.3c | 首轮创建 | `{"input_tokens": 2000, "cache_creation_input_tokens": 1800}` | 正确识别为缓存创建轮 |

### UT-10.4.4: 降级处理（Rust）

| ID | 场景 | 条件 | 期望结果 |
|----|------|------|----------|
| UT-10.4.4a | Provider 不支持 cache_control | LLM API 返回错误提示未知字段 | 自动重试不含 cache_control 的请求 |
| UT-10.4.4b | 缓存功能开关 | 配置 `enable_prompt_cache: false` | 不注入任何 cache_control 标记 |

---

## 集成测试 (IT)

### IT-10.4.1: 缓存命中验证

**前置条件**: DashScope qwen3.5-plus API 可用

**步骤**:
1. `start_preflight("实现用户认证系统")`
2. 等待第 1 轮 Agent 回复，记录 usage
3. 发送用户选择，等待第 2 轮回复，记录 usage
4. 对比两轮的 `cache_read_input_tokens`

**验证点**:
- [ ] 第 1 轮: `cache_creation_input_tokens > 0`，`cache_read_input_tokens = 0`
- [ ] 第 2 轮: `cache_read_input_tokens > 0`（缓存命中）
- [ ] 第 2 轮的 `cache_read_input_tokens ≥ 1000`（至少 system prompt 被缓存）
- [ ] 后端日志输出 `cache_hit_ratio`

### IT-10.4.2: 缓存键稳定性

**步骤**:
1. 连续 5 轮对话
2. 检查每轮日志中的缓存命中率

**验证点**:
- [ ] 第 2-5 轮均有缓存命中
- [ ] 缓存命中率逐轮递增或稳定
- [ ] 无"缓存被意外打碎"的情况（命中率突降）

### IT-10.4.3: 缓存过期验证

**步骤**:
1. 启动 Pre-flight，对话 2 轮
2. 等待 6 分钟不操作
3. 发送第 3 轮消息

**验证点**:
- [ ] 第 3 轮: `cache_read_input_tokens = 0`（缓存已过期）
- [ ] 第 3 轮: `cache_creation_input_tokens > 0`（重新创建缓存）
- [ ] 功能不受影响，对话正常继续

### IT-10.4.4: 非 DashScope Provider 降级

**步骤**:
1. 配置一个不支持 `cache_control` 的 LLM provider
2. 启动 Pre-flight

**验证点**:
- [ ] 不报错
- [ ] 对话功能正常
- [ ] 后端日志记录 `cache_control: disabled (unsupported provider)`

---

## 行为测试 (BT)

### BT-10.4.1: 成本节省实测

**目的**: 验证实际成本节省幅度

**步骤**:
1. 用"实现用户认证系统"完成 8 轮 Pre-flight，记录每轮 usage
2. 计算总有效 token 成本

**度量**:
| 轮次 | input_tokens | cache_read | cache_creation | 有效成本 (标准价=1) |
|------|-------------|------------|----------------|-------------------|
| 1 | T1 | 0 | C1 | T1 + C1×0.25 |
| 2 | T2 | R2 | C2 | (T2-R2) + R2×0.1 + C2×0.25 |
| ... | ... | ... | ... | ... |
| 总计 | ΣT | ΣR | ΣC | Σ有效 |

**通过标准**:
- 总有效成本 ≤ 无缓存总成本 × 0.45（即节省 ≥ 55%）

### BT-10.4.2: 延迟改善实测

**目的**: 验证 TTFT 改善

**步骤**:
1. 8 轮对话中记录每轮的 TTFT (Time To First Token)

**度量**:
| 指标 | 通过标准 |
|------|----------|
| 第 2-8 轮平均 TTFT vs 第 1 轮 TTFT | 降低 ≥ 15% |
| 第 2-8 轮 TTFT 标准差 | ≤ 500ms（稳定性） |

### BT-10.4.3: 对话质量无退化

**目的**: 确认缓存不影响 LLM 输出质量

**步骤**:
1. 对比开启/关闭缓存两组的 Contract 最终内容

**度量**:
| 维度 | 通过标准 |
|------|----------|
| Contract 条目数量差异 | ≤ 20% |
| 四区块覆盖率差异 | 0 |
| 对话自然度 | 无明显差异（人工评估） |

---

## 回归测试 (RT)

| ID | 验证项 | 方法 |
|----|--------|------|
| RT-10.4.1 | Quick Plan 不受影响 | Quick Plan 不使用 cache_control |
| RT-10.4.2 | Agent Stream (非 Pre-flight) 不受影响 | 正常 Agent 执行流程正常 |
| RT-10.4.3 | 配置中关闭缓存 | `enable_prompt_cache: false` → 无 cache_control 标记，功能正常 |
