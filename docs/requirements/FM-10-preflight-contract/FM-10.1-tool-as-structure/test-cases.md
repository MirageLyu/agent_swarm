# FM-10.1: Tool-as-Structure — 测试用例

> 版本: v1.0 | 日期: 2026-04-09

---

## 单元测试 (UT)

### UT-10.1.1: 工具 Schema 定义验证（Rust）

| ID | 场景 | 输入 | 期望结果 |
|----|------|------|----------|
| UT-10.1.1a | Schema 可序列化 | `PREFLIGHT_TOOLS` 常量 | `serde_json::to_value()` 成功，输出合法 JSON |
| UT-10.1.1b | Schema 符合 OpenAI 格式 | 序列化结果 | 每个工具含 `type: "function"`, `function.name`, `function.parameters` |
| UT-10.1.1c | Schema token 开销 | tiktoken 计算 | ≤ 1200 tokens |
| UT-10.1.1d | 5 个工具齐全 | 工具名列表 | 包含 `present_choices`, `add_contract_item`, `update_contract_item`, `suggest_sign`, `switch_clarification_mode` |

### UT-10.1.2: tool_call 解析（Rust）

| ID | 场景 | 输入 | 期望结果 |
|----|------|------|----------|
| UT-10.1.2a | 标准 present_choices | `{"name":"present_choices","arguments":"{\"question\":\"选择认证方式\",\"dimension\":\"scope\",\"choices\":[{\"id\":\"a\",\"label\":\"OAuth\"},...]}"}` | 解析为 `PresentChoices` 结构体，dimension=scope，choices.len() ≥ 2 |
| UT-10.1.2b | 标准 add_contract_item | `{"name":"add_contract_item","arguments":"{\"section\":\"scope\",\"item\":\"实现OAuth登录\",\"confidence\":\"confirmed\"}"}` | 解析为 `AddContractItem`，section=scope，confidence=confirmed |
| UT-10.1.2c | 标准 suggest_sign | `{"name":"suggest_sign","arguments":"{\"readiness_assessment\":{...},\"summary\":\"...\"}"}` | 解析为 `SuggestSign`，readiness_assessment 含三个字段 |
| UT-10.1.2d | arguments 为非法 JSON | `{"name":"present_choices","arguments":"not json"}` | 返回 Err，不 panic |
| UT-10.1.2e | 未知工具名 | `{"name":"unknown_tool","arguments":"{}"}` | 忽略并记录 warn 日志 |
| UT-10.1.2f | arguments 缺少必填字段 | `{"name":"present_choices","arguments":"{\"question\":\"q\"}"}` | 返回 Err（缺少 dimension 和 choices） |
| UT-10.1.2g | 多个 tool_calls | 响应含 2 个 tool_calls | 两个都被解析和处理 |

### UT-10.1.3: Fallback 解析链路（Rust）

| ID | 场景 | 输入 | 期望结果 |
|----|------|------|----------|
| UT-10.1.3a | 有 tool_calls → 不触发 fallback | 含 tool_calls 的响应 | `fallback_used = "none"` |
| UT-10.1.3b | 无 tool_calls + 有 CHOICES 分隔符 | `"文本\n---CHOICES---\n[{...}]"` | fallback 解析成功，`fallback_used = "text"` |
| UT-10.1.3c | 无 tool_calls + 无分隔符 + 有 Markdown 列表 | `"文本\n- **A. OAuth** - ...\n- **B. JWT** - ..."` | Markdown fallback 解析，`fallback_used = "markdown"` |
| UT-10.1.3d | 三层都失败 | `"纯文本回复无选项"` | 返回空 choices，不报错 |

### UT-10.1.4: tool_result 构建（Rust）

| ID | 场景 | 输入 | 期望结果 |
|----|------|------|----------|
| UT-10.1.4a | add_contract_item 成功 | 合法的 AddContractItem | 写入 DB + 返回 `{success: true, item_id: "xxx"}` |
| UT-10.1.4b | add_contract_item 重复 | 相同 section + item 文本 | 返回 `{success: false, reason: "duplicate"}` 或去重 |
| UT-10.1.4c | update_contract_item 条目不存在 | 不存在的 item_id | 返回 `{success: false, reason: "not_found"}` |
| UT-10.1.4d | switch_clarification_mode | `{mode: "devils_advocate"}` | session.mode 更新，返回 `{success: true}` |

---

## 集成测试 (IT)

### IT-10.1.1: 完整 tool_use 链路

**前置条件**: 已配置可用的 LLM API

**步骤**:
1. 调用 `start_preflight(description: "实现用户注册系统")`
2. 监听 `preflight-stream` 事件
3. 验证 Agent 首轮回复

**验证点**:
- [ ] Agent 回复中包含 `tool_calls` 字段
- [ ] `tool_calls` 中至少有一个 `present_choices` 调用
- [ ] `present_choices` 的 `dimension` 为合法枚举值
- [ ] `choices` 数组 ≥ 2 项，每项有 `id` + `label`
- [ ] 前端正确渲染选项按钮

### IT-10.1.2: tool_call → Contract 自动写入

**步骤**:
1. 启动 Pre-flight，等待 Agent 首轮回复
2. 用户选择一个选项
3. 发送用户选择
4. 观察 Agent 回复中是否包含 `add_contract_item` tool_call

**验证点**:
- [ ] `add_contract_item` 被调用至少 1 次
- [ ] DB 中 `contract_items` 表新增记录
- [ ] 前端 `ContractPanel` 实时更新显示新条目
- [ ] 条目的 `source` 为 `agent`

### IT-10.1.3: Fallback 链路端到端

**步骤**:
1. 临时修改 system prompt 不包含工具使用指引（模拟 LLM 不输出 tool_calls）
2. 发送消息触发 Agent 回复
3. 验证 fallback 解析生效

**验证点**:
- [ ] 后端日志记录 `fallback_used = "text"` 或 `"markdown"`
- [ ] 前端仍然渲染出选项按钮
- [ ] 用户体验与 tool_use 路径无差异

### IT-10.1.4: suggest_sign 端到端

**步骤**:
1. 进行多轮对话直到 Contract 较为完整
2. 等待 Agent 调用 `suggest_sign`

**验证点**:
- [ ] Agent 在合适时机调用 `suggest_sign`
- [ ] 前端展示签署确认 UI，包含 `readiness_assessment` 和 `summary`
- [ ] 用户拒绝签署时，Agent 继续提问
- [ ] 用户接受签署时，Contract 状态变为 `signed`

---

## 行为测试 (BT)

### BT-10.1.1: 工具遵从度批量验证

**目的**: 验证 qwen3.5-plus 在多种需求场景下的工具使用遵从度

**步骤**:
1. 准备 3 类需求场景：
   - 简单需求: "给项目添加 README"
   - 中等需求: "实现用户认证系统"
   - 复杂需求: "设计一个支持多租户的 SaaS 电商平台"
2. 每个场景运行 5 轮 Pre-flight 对话
3. 统计每轮的工具使用情况

**度量**:
| 指标 | 计算方式 | 通过标准 |
|------|----------|----------|
| 工具遵从率 | `使用 tool_use 的轮次 / 应包含选项的总轮次` | ≥ 87% (13/15) |
| 单问题率 | `仅含 1 个 present_choices 的轮次 / 含 present_choices 的轮次` | ≥ 90% |
| Schema 符合率 | `参数校验通过的 tool_call / 总 tool_call` | ≥ 95% |

### BT-10.1.2: 对话质量对比（A/B 测试）

**目的**: 验证 tool_use 方案不会降低对话质量

**步骤**:
1. 用"实现用户认证系统"分别运行 tool_use 路径和旧 text 路径各 3 次完整 Pre-flight
2. 对比两组的 Contract 最终内容

**度量**:
| 指标 | 计算方式 | 通过标准 |
|------|----------|----------|
| Contract 条目数量 | 最终 Contract 中的 items 总数 | tool_use 组 ≥ text 组的 80% |
| 四区块覆盖率 | 有 ≥1 条目的区块数 / 4 | tool_use 组 ≥ text 组 |
| 对话轮数 | 达到可签署状态的轮数 | tool_use 组 ≤ text 组 × 1.2 |

### BT-10.1.3: 降级无感验证

**目的**: 验证 Fallback 触发时用户无感知

**步骤**:
1. 手动构造 3 种 LLM 响应：
   - 仅含 tool_calls
   - 仅含 `---CHOICES---` 文本
   - 仅含 Markdown 列表
2. 分别注入到前端渲染

**度量**:
| 维度 | 通过标准 |
|------|----------|
| UI 一致性 | 3 种路径的选项按钮样式、交互完全一致 |
| 性能差异 | 渲染延迟差异 < 50ms |
| 错误提示 | 均无错误提示/异常 UI |

---

## 回归测试 (RT)

### RT-10.1.1: 现有功能不退化

| ID | 验证项 | 方法 |
|----|--------|------|
| RT-10.1.1a | Quick Plan 流程正常 | 选择 Quick Plan → DAG 正常生成 |
| RT-10.1.1b | 用户自由输入不受影响 | Pre-flight 中不选择选项，直接输入文字回复 → Agent 正常响应 |
| RT-10.1.1c | Contract 手动编辑不受影响 | 手动添加/删除 Contract 条目 → 功能正常 |
| RT-10.1.1d | 签署流程正常 | 签署 Contract → DAG 生成 → 跳转 MissionsView |


TMP

测试步骤：

下面是完整的手动集成测试操作清单。

---

## 前置准备

1. **删除旧数据库**（触发迁移 010）

```bash
rm -f ~/Library/Application\ Support/com.miragenty.app/miragenty.db
```

2. **启动应用**

```bash
cd /Volumes/T7/Miragenty && pnpm tauri dev
```

3. **确认 Settings 中 API Key 已配置**（DashScope qwen3.5-plus）

4. **打开开发者工具**
   - 前端：`Cmd+Option+I` 查看 Console
   - 后端：观察终端中的 `tracing` 日志输出

---

## 测试 1：完整 tool_use 链路 (IT-10.1.1)

| 步骤 | 操作 | 验证 |
|------|------|------|
| 1 | 点击「新建任务」→ 输入 "实现用户注册系统" → 启动 Pre-flight | 进入 PreflightView 双栏布局 |
| 2 | 等待 Agent 首轮回复完成 | 观察后端日志 |
| 3 | — | ✅ 日志中出现 `tool_calls_count = N`（N ≥ 1） |
| 4 | — | ✅ 日志中出现 `fallback_used = "none"` |
| 5 | — | ✅ 前端渲染出 ≥ 2 个选项按钮 |
| 6 | — | ✅ 状态栏显示「探索」阶段标签，蓝色进度条 |

**如果 tool_use 失败（fallback_used ≠ "none"）：** 这也是合法结果，说明 qwen3.5-plus 在该轮未使用 tool_call，fallback 链路正在工作。记录下来统计遵从率。

---

## 测试 2：tool_call → Contract 自动写入 (IT-10.1.2)

| 步骤 | 操作 | 验证 |
|------|------|------|
| 1 | 接上一个测试，选择第一个选项 | 选项按钮变为选中态，消息发送 |
| 2 | 等待 Agent 第二轮回复 | — |
| 3 | — | ✅ 后端日志出现 `tool_names` 中包含 `add_contract_item` |
| 4 | — | ✅ 右侧 ContractPanel 实时出现新条目（无需手动刷新） |
| 5 | — | ✅ 新条目出现在对应区块（scope/constraints/...） |
| 6 | 查 DB 验证 | `sqlite3 ~/Library/Application\ Support/com.miragenty.app/miragenty.db "SELECT id, section, text, source FROM contract_items ORDER BY created_at DESC LIMIT 5"` → source 列为 `agent` |

---

## 测试 3：Belief State 全链路 (IT-10.2.1)

| 步骤 | 操作 | 验证 |
|------|------|------|
| 1 | 继续对话，共完成 3 轮选择 | 每轮选择一个选项 |
| 2 | 观察每轮日志 | ✅ `convergence_score` 逐轮递增（或不减） |
| 3 | — | ✅ `phase` 从 `exploring` 变为 `narrowing`（score ≥ 0.3 时） |
| 4 | 观察前端状态栏 | ✅ 进度百分比在连续增长（不是 5 档跳跃） |
| 5 | — | ✅ 阶段标签从「探索」变为「收窄」，颜色从蓝变紫 |
| 6 | 查 DB 验证 | `sqlite3 ...db "SELECT convergence_score, phase, belief_state FROM preflight_sessions ORDER BY updated_at DESC LIMIT 1"` → 验证 JSON 中 slots 状态更新 |

---

## 测试 4：Fallback 链路端到端 (IT-10.1.3)

> 这个测试需要模拟 LLM 不返回 tool_calls 的情况。

| 步骤 | 操作 | 验证 |
|------|------|------|
| 1 | 在 Agent 回复后，直接输入自由文本（如 "我想用 JWT 做认证"）而不选择选项 | 消息正常发送 |
| 2 | 如果 Agent 这轮没使用 tool_call（看日志） | ✅ `fallback_used = "text"` 或 `"markdown"` |
| 3 | — | ✅ 前端仍渲染出选项按钮 |
| 4 | — | ✅ 选项按钮样式与 tool_use 路径无差异 |

**备选方法：** 如果 LLM 总是使用 tool_call，可临时修改 `planner.rs` 中 `preflight_tools()` 返回 `vec![]`，重新编译后测试 fallback 链路。测完后恢复。

---

## 测试 5：模式切换 (RT-10.1.1b + switch_clarification_mode)

| 步骤 | 操作 | 验证 |
|------|------|------|
| 1 | 点击模式切换按钮 → 切换到「魔鬼代言人」 | ✅ 出现分隔线消息"── 切换到「魔鬼代言人」模式 ──" |
| 2 | 等待 Agent 回复 | ✅ Agent 回复风格转为挑战性提问 |
| 3 | 切换到「风险标记」 | ✅ Agent 回复风格转为风险分析 |
| 4 | 自由输入文字回复 | ✅ Agent 正常响应（RT-10.1.1b：自由输入不受影响） |

---

## 测试 6：签署流程 (IT-10.1.4 + IT-10.2.2)

| 步骤 | 操作 | 验证 |
|------|------|------|
| 1 | 持续对话直到右侧 Contract 有多个条目（至少 scope 区有 1 个） | — |
| 2 | 观察进度条 | ✅ 当 score ≥ 0.85 时，阶段显示「就绪」金色 |
| 3 | 如果 Agent 调用了 `suggest_sign` | ✅ 日志中出现 `suggest_sign`，前端收到事件 |
| 4 | 点击右侧 ContractPanel 底部「签署合同并启动 Swarm」按钮 | — |
| 5 | — | ✅ 进入 Planning 流程，Planner streaming 开始 |
| 6 | — | ✅ 最终跳转到 MissionsView，任务 DAG 正常生成 |

---

## 测试 7：简单需求快速路径 (IT-10.2.3)

| 步骤 | 操作 | 验证 |
|------|------|------|
| 1 | 新建 Pre-flight → 输入 "给项目添加 README 文件" | — |
| 2 | 每轮选择 Agent 推荐的选项 | 记录每轮 convergence_score |
| 3 | — | ✅ ≤ 4 轮到达 `phase = ready_to_sign` |
| 4 | — | ✅ `convergence_score ≥ 0.85` |

---

## 测试 8：回归验证 (RT)

| ID | 操作 | 验证 |
|----|------|------|
| RT-10.1.1a | 关闭 Pre-flight，从 MissionsView 用 Quick Plan 新建任务 | ✅ DAG 正常生成，不受 preflight 改动影响 |
| RT-10.1.1c | 在 Pre-flight 中，手动在 Contract 中添加/删除条目 | ✅ 功能正常 |
| RT-10.2.1 | 打开一个旧 session（无 belief_state 列的）或清空 belief_state | ✅ 不报错，自动初始化默认状态 |
| RT-10.2.3 | Quick Plan 流程 | ✅ 无 belief_state 参与 |

---

## 日志关键字速查

后端日志中搜索这些关键字可快速定位：

| 关键字 | 含义 |
|--------|------|
| `preflight round completed` | 每轮完成的结构化日志（含 score/phase/tool_calls_count） |
| `tool_calls_count` | 该轮 tool_call 数量 |
| `fallback_used` | `none` = tool_use 路径，`text` = CHOICES 分隔符，`markdown` = Markdown 提取 |
| `convergence_score` | 当前收敛分数 |
| `phase` | 当前对话阶段 |
| `Failed to parse` | tool_call 参数解析失败（需关注） |
| `Unknown preflight tool` | LLM 调用了未知工具（需关注） |

---

## DB 查询速查

```bash
DB=~/Library/Application\ Support/com.miragenty.app/miragenty.db

# 查看 belief_state
sqlite3 "$DB" "SELECT id, convergence_score, phase, belief_state FROM preflight_sessions ORDER BY updated_at DESC LIMIT 1"

# 查看 agent 写入的 contract items
sqlite3 "$DB" "SELECT section, text, source FROM contract_items WHERE source='agent' ORDER BY created_at DESC LIMIT 10"

# 查看消息历史中的 tool_calls
sqlite3 "$DB" "SELECT json_extract(value, '$.role'), json_extract(value, '$.tool_calls') FROM preflight_sessions, json_each(messages) WHERE json_extract(value, '$.tool_calls') IS NOT NULL LIMIT 5"
```
