# FM-10.3 ~ FM-10.6 手动验证 Checklist

> 日期: 2026-04-10
> 前置条件: `RUST_LOG=miragenty=debug cargo tauri dev`
> 测试场景: 输入「实现一个用户认证系统，支持邮箱注册、OAuth 第三方登录、密码重置、RBAC 权限控制」

---

## Phase 1: 启动 & 首轮对话 (Round 1)

### V-01: Tool-as-Structure 基础功能不回归

| 项目 | 内容 |
|------|------|
| **操作** | 在 Pre-flight 页面输入上述需求描述，点击发送 |
| **预期** | Agent 首轮回复通过 `present_choices` 工具呈现 ≥ 2 个结构化选项按钮，不是纯文本列表 |
| **日志验证** | 终端出现 `preflight tool_calls parsed tool_calls_count=1 tool_names="present_choices"` |
| **通过?** | ☐ |

### V-02: Token 用量持久化

| 项目 | 内容 |
|------|------|
| **操作** | 观察首轮完成后的日志 |
| **预期** | 日志出现 `preflight stream complete ... input_tokens=XXXX output_tokens=XXXX`，两个值均 > 0 |
| **日志验证** | `input_tokens` 值通常在 1200-2000 范围（system prompt + tools + 用户消息） |
| **通过?** | ☐ |

### V-03: 收敛状态初始化

| 项目 | 内容 |
|------|------|
| **操作** | 观察页面底部 `PreflightStatusBar` |
| **预期** | 显示蓝色「探索」标签，收敛进度条 0%，提示文本为「5 分钟澄清 → 节省 ~$50 错误方向成本」 |
| **通过?** | ☐ |

### V-04: 决策时间线组件存在

| 项目 | 内容 |
|------|------|
| **操作** | 在右侧 Contract 面板底部，找到「▶ 决策时间线 0」按钮 |
| **预期** | 按钮存在且可点击；点击展开后显示「尚无决策记录」空状态；再次点击收起 |
| **通过?** | ☐ |

---

## Phase 2: 选项选择 & 合同条目积累 (Round 2-3)

### V-05: add_contract_item 自动触发

| 项目 | 内容 |
|------|------|
| **操作** | 点击 Agent 呈现的某个选项按钮（例如选择「支持 OAuth + 邮箱注册」） |
| **预期** | Agent 回复中自动调用 `add_contract_item`，右侧 Contract 面板的 Scope 区块出现新条目 |
| **日志验证** | 日志出现 `tool_names="add_contract_item"` 然后紧接 `preflight continuing with tool_results` |
| **通过?** | ☐ |

### V-06: 决策自动记录 — Confirmed 类型

| 项目 | 内容 |
|------|------|
| **操作** | 展开决策时间线（点击「▶ 决策时间线」） |
| **预期** | 出现 ≥ 1 条记录，绿色「确认」badge，描述内容与刚才写入 Contract 的条目一致，显示 `R1` 或 `R2` 轮次 |
| **通过?** | ☐ |

### V-07: 决策时间线浮层不挤压 Grid

| 项目 | 内容 |
|------|------|
| **操作** | 展开决策时间线，观察上方 2×2 Contract 区块 |
| **预期** | 决策时间线作为半透明浮层覆盖在 grid 上方，有阴影分层效果；grid 本身不发生位移或压缩 |
| **通过?** | ☐ |

### V-08: 收敛分数递增

| 项目 | 内容 |
|------|------|
| **操作** | 第 2 轮完成后观察底部 StatusBar |
| **预期** | 收敛进度 > 0%（通常 5%-15%），日志中 `convergence_score` 值 > 0 |
| **日志验证** | `preflight round completed ... convergence_score=0.XX phase=exploring` |
| **通过?** | ☐ |

---

## Phase 3: 继续对话 + 否决方案 (Round 3-5)

### V-09: 用户否决方案

| 项目 | 内容 |
|------|------|
| **操作** | 当 Agent 提供选项时，在输入框手动回复「不需要生物识别登录，我只要邮箱和 OAuth」 |
| **预期** | Agent 接受用户意见，不再坚持生物识别，转向下一个话题 |
| **通过?** | ☐ |

### V-10: Dynamic Prompt — Contract 状态注入

| 项目 | 内容 |
|------|------|
| **操作** | 在第 5 轮对话时输入「请列出 Contract 中目前有哪些已确认的条目」 |
| **预期** | Agent 能正确引用右侧 Contract 面板中 ≥ 80% 的已确认条目（逐条对比） |
| **通过?** | ☐ |

### V-11: Micro-compact 启动

| 项目 | 内容 |
|------|------|
| **操作** | 继续对话到第 4-5 轮，观察终端日志 |
| **预期** | 出现 `micro-compact applied to message history` 日志行 |
| **日志验证** | `grep "micro-compact applied"` 命中 |
| **通过?** | ☐ |

---

## Phase 4: 模式切换 (Round 6-7)

### V-12: 切换到魔鬼代言人模式

| 项目 | 内容 |
|------|------|
| **操作** | 点击聊天区上方的模式切换器，选择「魔鬼代言人」 |
| **预期 1** | 聊天区出现分隔消息「── 切换到「魔鬼代言人」模式 ──」 |
| **预期 2** | Agent 自动开始新一轮对话，风格转为质疑和挑战（如「如果 OAuth 提供商宕机怎么办」） |
| **日志验证** | `mode_switched` 事件出现在日志中 |
| **通过?** | ☐ |

### V-13: 模式切换后 Agent 不再场景走查风格

| 项目 | 内容 |
|------|------|
| **操作** | 继续回复 1-2 轮 |
| **预期** | Agent 保持质疑风格，不会回到温和的场景描述方式 |
| **通过?** | ☐ |

---

## Phase 5: 否决方案不重复 (Round 7-12)

### V-14: 否决方案注入 Prompt 验证

| 项目 | 内容 |
|------|------|
| **操作** | 在 Phase 3 已否决「生物识别登录」后，继续对话 5 轮（Round 7-12），观察 Agent 是否重提 |
| **预期** | 5 轮内 Agent 不再建议「生物识别」或「指纹/面容识别」相关方案 |
| **通过?** | ☐ |

### V-15: 决策时间线条目持续积累

| 项目 | 内容 |
|------|------|
| **操作** | 展开决策时间线 |
| **预期** | 随着对话推进，时间线中的记录数 ≥ 3 条，包含绿色「确认」和/或橙色「修订」类型，每条显示对应轮次 |
| **通过?** | ☐ |

---

## Phase 6: Full Compaction (Round 12+)

### V-16: Full Compaction 触发

| 项目 | 内容 |
|------|------|
| **操作** | 持续对话到第 12 轮（可以发送简短确认加速推进，如「好的」「同意」「继续下一个问题」） |
| **预期** | 前端短暂出现「正在优化对话上下文…」状态提示 |
| **日志验证** | 终端出现 `triggering full compaction` 和 `full compaction succeeded summary_len=XXX` |
| **通过?** | ☐ |

### V-17: Compaction 后 Agent 保留关键记忆

| 项目 | 内容 |
|------|------|
| **操作** | Compaction 完成后，输入「我们之前确认了哪些功能范围？有哪些被我否决的方案？」 |
| **预期 1** | Agent 能列出 ≥ 2 个已确认的 Scope 条目 |
| **预期 2** | Agent 能提到 ≥ 1 个被否决的方案（如「生物识别登录」） |
| **通过?** | ☐ |

### V-18: 长对话稳定性

| 项目 | 内容 |
|------|------|
| **操作** | 继续对话到第 15 轮 |
| **预期** | 无报错，Agent 正常回复，前端正常渲染 |
| **通过?** | ☐ |

---

## Phase 7: 收敛 & 阶段转移

### V-19: 阶段标签变化

| 项目 | 内容 |
|------|------|
| **操作** | 观察整个对话过程中底部 StatusBar 的阶段标签 |
| **预期** | 从蓝色「探索」逐步变为紫色「收窄」→ 绿色「确认」（收敛分到达阈值时自动切换） |
| **日志验证** | `preflight round completed ... phase=narrowing` 或 `phase=confirming` 出现 |
| **通过?** | ☐ |

### V-20: ReadyToSign 阶段 Agent 建议签署

| 项目 | 内容 |
|------|------|
| **操作** | 如果收敛分 > 85%，观察 Agent 行为 |
| **预期** | Agent 使用 `suggest_sign` 工具主动建议签署 Contract，或在文本中明确建议「可以签署合同了」 |
| **日志验证** | 日志出现 `tool_names="suggest_sign"` 或 `suggest_sign received` |
| **备注** | 此项需要足够多的 confirmed 条目；如果对话轮次不够，可手动在 Contract 面板添加条目加速收敛 |
| **通过?** | ☐ |

---

## Phase 8: Prompt Caching 验证

> **注意**: 此 Phase 当前可能因 DashScope API 格式兼容性问题而无法通过（cache_control 层级待确认）。记录实际表现即可。

### V-21: 缓存指标日志

| 项目 | 内容 |
|------|------|
| **操作** | 在终端日志中搜索 `preflight cache metrics` |
| **预期（理想）** | 第 2 轮起出现此日志，`cache_read_tokens > 0`，`cache_hit_ratio ≥ 0.40` |
| **预期（当前）** | 该行不出现 = DashScope 未返回 cache 指标，`cache_control` 被静默忽略 |
| **日志验证** | `grep "preflight cache metrics"` |
| **实际表现** | ☐ 理想  ☐ 当前（降级正常，待后续修复） |

### V-22: 非 DashScope Provider 降级不报错

| 项目 | 内容 |
|------|------|
| **操作** | （可选）在设置页将 `base_url` 临时改为其他 OpenAI-compatible 地址，发送一轮对话 |
| **预期** | 对话正常完成，无 `cache_control` 相关报错 |
| **通过?** | ☐ |

---

## 日志速查命令

```bash
# 在 cargo tauri dev 的终端中，对话结束后执行：

# FM-10.3: 动态 Prompt — 查看每轮的 phase 和 convergence_score
grep "preflight round completed" /tmp/miragenty.log

# FM-10.4: 缓存命中
grep "preflight cache metrics" /tmp/miragenty.log

# FM-10.5: 微压缩
grep "micro-compact applied" /tmp/miragenty.log

# FM-10.5: 全量压缩
grep "full compaction" /tmp/miragenty.log

# FM-10.6: 决策记录写入
grep "insert_decision_entry\|decision_log" /tmp/miragenty.log

# 全局异常
grep "ERROR\|WARN" /tmp/miragenty.log
```

> 注: 如果日志直接输出到 stderr 而非文件，请直接在终端中观察或用 `cargo tauri dev 2>&1 | tee /tmp/miragenty.log` 重定向。

---

## 汇总

| Phase | 检查项 | 覆盖需求 | 通过数 |
|-------|--------|----------|--------|
| 1 | V-01 ~ V-04 | FM-10.1 回归, FM-10.3 初始化, FM-10.5 token 追踪, FM-10.6 UI | /4 |
| 2 | V-05 ~ V-08 | FM-10.1 回归, FM-10.6 自动记录, FM-10.6 UI 浮层, FM-10.3 收敛 | /4 |
| 3 | V-09 ~ V-11 | FM-10.6 否决, FM-10.3 Contract 注入, FM-10.5 micro-compact | /3 |
| 4 | V-12 ~ V-13 | FM-10.3 模式切换 | /2 |
| 5 | V-14 ~ V-15 | FM-10.6 否决不重复, FM-10.6 时间线积累 | /2 |
| 6 | V-16 ~ V-18 | FM-10.5 Full Compaction + 降级 + 稳定性 | /3 |
| 7 | V-19 ~ V-20 | FM-10.3 阶段转移 + suggest_sign | /2 |
| 8 | V-21 ~ V-22 | FM-10.4 缓存指标 + Provider 降级 | /2 |
| **合计** | **V-01 ~ V-22** | **FM-10.3 ~ FM-10.6** | **/22** |
