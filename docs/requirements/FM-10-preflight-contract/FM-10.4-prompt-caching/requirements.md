# FM-10.4: Prompt Caching — 提示缓存

> 版本: v1.0 | 日期: 2026-04-09  
> 优先级: **P1** | 预估周期: 1 天  
> 依赖: FM-10.3 (Dynamic System Prompt — 静态/动态分区) | 被依赖: 无（独立优化）  
> 调研来源: Claude Code 架构分析 §维度7; DashScope 官方文档 (Context Cache)

---

## 1. 目标

利用 DashScope qwen3.5-plus 原生支持的显式 Prompt Caching（兼容 Anthropic `cache_control` 语法），将 Pre-flight 多轮对话中重复传输的 system prompt 和工具定义缓存化，实现：

1. **成本降低 ~78%**：缓存命中时输入 token 成本降为标准价的 10%
2. **延迟降低**：缓存命中时跳过 KV Cache 重算，首 token 延迟显著降低
3. **无功能变化**：纯性能优化，不改变对话行为

---

## 2. 技术背景

### DashScope 显式缓存参数

| 参数 | 值 |
|------|-----|
| 创建成本 | 标准输入价 × 125% |
| 命中成本 | 标准输入价 × **10%** (1 折) |
| 有效期 | 5 分钟（每次命中自动续期） |
| 每请求最多标记数 | 4 个 `cache_control` 标记 |
| 最低可缓存 token | 1024 tokens |
| 可缓存内容 | system / user / assistant / tool 消息 |
| 语法 | `cache_control: { type: "ephemeral" }`（兼容 Anthropic） |

### Pre-flight 场景分析

典型 Pre-flight 会话：8 轮对话，每轮请求包含：
- System prompt: ~1500 tokens (含静态前缀 ~500 + 动态后缀 ~1000)
- 工具定义: ~1000 tokens
- 对话历史: 递增（第 8 轮 ~4000 tokens）

8 轮对话无缓存总输入 token ≈ 8 × (1500 + 1000) + Σ(历史) ≈ 36,000 tokens  
缓存命中后总有效输入 token ≈ 8 × (500×0.1 + 1000×0.1 + 1000) + Σ(历史) ≈ 10,880 tokens

**预计成本节省 ≈ 70%**

---

## 3. 功能需求

### FR-10.4.1: System Prompt 缓存标记

- **FR-10.4.1a**: 在 system prompt 的静态前缀末尾（`__DYNAMIC_BOUNDARY__` 之前）添加 `cache_control: { type: "ephemeral" }` 标记
- **FR-10.4.1b**: 静态前缀的 token 数必须 ≥ 1024（DashScope 最低缓存阈值），不足时用空白填充或合并动态段的稳定部分
- **FR-10.4.1c**: 静态前缀在整个 session 中字节级一致（FM-10.3 保证），确保缓存键稳定

### FR-10.4.2: 工具定义缓存标记

- **FR-10.4.2a**: 在 LLM 请求的 `tools` 参数中，最后一个工具定义上添加 `cache_control` 标记
- **FR-10.4.2b**: 工具定义顺序在整个 session 中保持固定（排序稳定性）
- **FR-10.4.2c**: 工具定义不因模式切换或轮次变化而改变（5 个工具始终全量传入）

### FR-10.4.3: 对话历史滚动断点

- **FR-10.4.3a**: 每轮请求中，在最后一条 user 消息上添加 `cache_control` 标记
- **FR-10.4.3b**: 这确保从 system prompt 到最后一条 user 消息的整段可被缓存

### FR-10.4.4: OpenAI-compatible API 适配

在 `llm/openai_compat.rs` 的请求构建中：

- **FR-10.4.4a**: 新增 `cache_control` 字段支持到 `Message` 类型
- **FR-10.4.4b**: `stream_chat()` 构建请求时，按标记策略注入 `cache_control`
- **FR-10.4.4c**: 解析响应中的 `usage.cache_read_input_tokens` 和 `usage.cache_creation_input_tokens` 字段

### FR-10.4.5: 缓存效果监控

- **FR-10.4.5a**: 每轮请求后记录缓存指标到结构化日志：
  ```
  tracing::info!(
      cache_creation_tokens = %creation,
      cache_read_tokens = %read,
      total_input_tokens = %total,
      cache_hit_ratio = %ratio,
      "preflight cache metrics"
  );
  ```
- **FR-10.4.5b**: `cache_hit_ratio = cache_read_tokens / total_input_tokens`
- **FR-10.4.5c**: 前端 `PreflightStatusBar` 可选显示缓存命中率（调试模式）

### FR-10.4.6: 缓存预热（可选）

- **FR-10.4.6a**: 在 `start_preflight` 时，发送一个 `max_tokens: 1` 的 dummy 请求，仅包含 system prompt + 工具定义，用于预热 KV Cache
- **FR-10.4.6b**: Dummy 请求与 session 初始化并行执行，不增加用户感知延迟
- **FR-10.4.6c**: 此功能默认关闭，通过配置 `preflight_cache_warmup: true` 开启

---

## 4. 非功能需求

| ID | 类型 | 要求 |
|----|------|------|
| NFR-10.4.1 | 兼容性 | 必须兼容 DashScope OpenAI-compatible API 的 `cache_control` 语法 |
| NFR-10.4.2 | 降级 | 不支持缓存的 LLM provider 应自动忽略 `cache_control` 字段，不报错 |
| NFR-10.4.3 | 性能 | 缓存标记注入不增加请求构建延迟（< 1ms） |
| NFR-10.4.4 | 可观测性 | 必须有缓存命中率日志，支持效果评估 |

---

## 5. 效果度量

### 5.1 定量指标

| 指标 | 度量方法 | 基线（当前） | 目标 | 采集方式 |
|------|----------|-------------|------|----------|
| **缓存命中率** | `cache_read_tokens / total_input_tokens`（第 2 轮起） | 0%（无缓存） | ≥ 60%（第 2 轮起） | 后端日志：解析 API 响应 usage |
| **每轮有效输入 token 成本** | `标准 token × 1.0 + 缓存命中 token × 0.1` | 全价 | 第 2 轮起降低 ≥ 50% | 后端日志计算 |
| **8 轮对话总成本** | 计算 8 轮的总有效输入 token 费用 | ~36K tokens 全价 | ≤ 15K tokens 等效价 | 后端日志累计 |
| **首 token 延迟 (TTFT)** | 从发送请求到收到首个 stream chunk 的时间 | 基线值 | 第 2 轮起降低 ≥ 20% | 后端计时 |
| **缓存创建开销** | 首轮的 `cache_creation_input_tokens × 1.25` | 0 | ≤ 2500 tokens × 1.25 = 3125 等效价 | 后端日志 |

### 5.2 缓存命中率分轮分析

| 轮次 | 预期缓存内容 | 预期命中率 |
|------|-------------|-----------|
| 第 1 轮 | 无缓存（首次创建） | 0% |
| 第 2 轮 | system prompt 静态前缀 + 工具定义 | ~40-50% |
| 第 3 轮 | 上述 + 前 2 轮对话历史 | ~50-60% |
| 第 4-8 轮 | 上述 + 累积对话历史 | ~60-70% |

### 5.3 验证实验

**实验设计**:
1. 准备"实现用户认证系统" Pre-flight 场景
2. 运行两组：
   - A组（开启缓存）: 3 次 × 8 轮
   - B组（关闭缓存）: 3 次 × 8 轮
3. 记录每轮的 usage 和计时数据

**对比维度**:
| 维度 | A组(缓存) | B组(无缓存) | 度量 |
|------|---------|-----------|------|
| 总输入 token 费用 | 预期降低 60%+ | 基线 | API usage |
| 平均 TTFT | 预期降低 20%+ | 基线 | 后端计时 |
| 对话质量 | 无差异 | 基线 | 人工评估 Contract 完成度 |

### 5.4 缓存失效场景验证

| 场景 | 期望行为 | 验证方式 |
|------|----------|----------|
| 轮次间隔 < 5 分钟 | 缓存命中 | 正常对话速度即可 |
| 轮次间隔 > 5 分钟 | 缓存失效，自动重建 | 人为延迟 6 分钟后发送消息 |
| 不支持缓存的 provider | `cache_control` 被忽略，功能正常 | 切换到非 DashScope provider 测试 |

---

## 6. 实现要点

### 6.1 后端改动

| 文件 | 改动 |
|------|------|
| `llm/types.rs` | `Message` 类型新增 `cache_control: Option<CacheControl>` 字段 |
| `llm/openai_compat.rs` | 请求构建时注入 `cache_control` 标记；解析响应 usage 中的缓存指标 |
| `agent/planner.rs` | `preflight_chat()` 在构建消息时按策略添加 `cache_control` |
| `commands/preflight.rs` | 可选的缓存预热 dummy 请求 |

### 6.2 缓存标记策略代码

```rust
fn apply_cache_markers(messages: &mut Vec<Message>, tools: &mut Vec<Tool>) {
    // 标记 1: system prompt 静态前缀末尾
    if let Some(system) = messages.iter_mut().find(|m| m.role == "system") {
        system.cache_control = Some(CacheControl::ephemeral());
    }
    
    // 标记 2: 工具定义最后一个
    if let Some(last_tool) = tools.last_mut() {
        last_tool.cache_control = Some(CacheControl::ephemeral());
    }
    
    // 标记 3: 最后一条 user 消息
    if let Some(last_user) = messages.iter_mut().rev().find(|m| m.role == "user") {
        last_user.cache_control = Some(CacheControl::ephemeral());
    }
}
```

---

## 7. 风险

| 风险 | 概率 | 影响 | 缓解 |
|------|------|------|------|
| DashScope API 不接受 `cache_control` 字段 | 低（文档明确支持） | 中 | 测试验证；不支持时 graceful 降级 |
| 缓存有效期 5 分钟太短 | 中（用户可能思考较久） | 低 | 每次命中续期；失效后自动重建 |
| 首轮创建成本 125% 导致短会话不划算 | 低（Pre-flight 通常 ≥ 5 轮） | 低 | 仅 ≥ 3 轮时开启缓存 |
| 缓存指标 API 字段不兼容 | 中 | 低 | 解析失败时忽略，不影响功能 |
