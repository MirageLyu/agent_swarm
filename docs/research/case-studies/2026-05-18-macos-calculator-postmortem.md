# Case Study: macOS Calculator Mission 全流程复盘

> **日期**: 2026-05-18
> **Mission**: macOS Calculator with Scientific Mode and Memory
> **Mission ID**: `26d64c5d-9347-4a9d-b8ae-fdaa81ddd7bf`
> **最终状态**: `failed`（tester 三次未通过，用户主动放弃）
> **目的**: 作为优化 Miragenty 全流程的事实依据
> **配套优化提案**: [`2026-05-18-macos-calculator-optimizations.md`](./2026-05-18-macos-calculator-optimizations.md)

本文档是**纯事实归档**，不含判断和建议。所有数据来自 `~/Library/Application Support/com.miragenty.app/miragenty.db` 和 `~/miragenty-workspaces/macos-calculator-with-scientific-mode-an-26d64c5d/` 的 git 物理状态，截取时间 2026-05-18 16:30。

---

## TL;DR

- **总体结论**：跑了 7 天，9 个 implementer/integrator/architect 全部完成，唯独 tester 三次启动三次失败/取消，mission 终态 `failed`。
- **直接卡点**：tester 任务先后撞上 ① max_steps=80 耗尽 ② wall_clock=1800s 超时 ③ 用户主动取消。第二次本质原因是 implementer 在 `factorialOf(n)` 里写了 `2...intN` range（n<2 时 fatalError），tester 想覆盖 `0!/1!` 时反复触发崩溃 + 修测试绕开 + 再撞别的对齐问题，在 30 分钟内没收敛。
- **结构性问题**：preflight 在 `convergence=0.64 / phase=narrowing / round=23 (max=12)` 时就签约；contract 17 条 items 全部由 agent 写，0 条由用户写；mission `directives` 字段记录了"自动化测试可以简化，快速完成任务优先"，这条内容是**运行时**通过 FM-06 Runtime Intervention 写入（不是 mission 创建时设的，详见 §3.4）——意味着 contract 签约 + planner 拆 task 时它还不存在；后来的 tester#2/#3 重启时 scheduler **确实**把它拼进了 agent prompt，但 agent 仍未真正贯彻"简化"导向。
- **资源画像**：累计 14.0M tokens 跨 10 个 worker agent（不含 preflight 360K + planning 520K），磁盘上保留 12 个 worktree 共 1.0 GB。

---

## 0. Mission 元数据

| 字段 | 值 |
| --- | --- |
| `title` | macOS Calculator with Scientific Mode and Memory |
| `description` | 开发一个macOS上的计算器应用，UI风格与macOS设计风格一致，且支持计算结果memory功能 |
| `directives` | **自动化测试可以简化，快速完成任务优先** ⚠️ 这是运行时（FM-06）写入的，**非** mission 创建时设置；详见 §3.4 |
| `repo_origin` | `from_scratch` |
| `merge_strategy` | `theirs` |
| `total_cost_usd` | 0.0 (DB 字段未被更新) |
| `created_at` | 2026-05-11 15:55:57 (CST) |
| `updated_at` | 2026-05-18 16:27:00 (CST，最后一次状态变更 = tester#3 取消) |
| `status` | `failed` |

---

## 1. 阶段时间轴（粗粒度）

```
2026-05-11 15:55  mission 创建 + preflight session 启动
2026-05-11 17:56  planner 第 1 次启动 → 180s 超时失败（15 steps）
2026-05-12 16:21  preflight 进入 round 23（max_rounds=12，已超 11 轮）
2026-05-12 16:22  planner 第 2 次启动 (孤儿)
2026-05-12 16:22  planner 第 3 次启动（晚 26 秒）→ 5 分 36 秒后完成，contract 签约
2026-05-12 16:29  planner 第 2 次完成（已无人接收，孤儿运行了 8 分钟，348K tokens 浪费）
2026-05-16 17:51  architect (ba6b3b57) 完成（耗时 7 分钟）
~~~~~~~~~~~~~~~  ── 中间停滞约 42 小时（用户未触发后续）──
2026-05-18 11:34  8 个 implementer + 1 integrator 开始并发跑（Wave 1-3）
2026-05-18 12:44  integrator (8f493d02) 完成 → tester (22d4be5a) 转 ready
2026-05-18 12:50  tester#1 (ab2ba0aa) 在 ~step 80 触发 max_steps 上限 → failed
2026-05-18 14:20  scheduler 自动 retry → tester#2 (8e4a8fc5) 启动
2026-05-18 14:50  tester#2 wall_clock 1800s 超时 → failed，mission failed
2026-05-18 15:50  开发者侧外部修复（详见 §7）
2026-05-18 16:07  用户在 UI 上 restart → tester#3 (2b8c40ee) 启动
2026-05-18 16:26  用户手动取消 tester#3 → status cancelled
2026-05-18 16:27  mission 终态 `failed`
```

---

## 2. 阶段 1：Preflight（5/11 ~ 5/12，约 24 小时）

### 2.1 Session 元数据

| 字段 | 值 |
| --- | --- |
| `id` | `2cf05a56-4c02-4792-b6d4-d256bce3b106` |
| `mode` | `risk_highlighter` |
| `phase` | `narrowing`（未达 `ready_to_sign`） |
| `convergence_score` | **0.64** |
| `cumulative_input_tokens` | 361,060 |
| `cumulative_output_tokens` | 26,516 |
| `msg_count` | **102** (assistant 40 / tool 41 / user **21**) |
| `compaction_failures` | 0（至少触发过 1 次成功压缩）|

### 2.2 Belief State 终态（关键槽位）

| Slot | Status | Confirmed at round |
| --- | --- | --- |
| `key_features` | confirmed | 2 |
| `target_users` | confirmed | 4 |
| `tech_constraints` | confirmed | 7 |
| `out_of_scope` | confirmed | 9 |
| `risk_assumptions` | tentative | 10 |
| `integration_points` | confirmed | 11 |
| `security_requirements` | confirmed | 23 |
| **`timeline_budget`** | **unfilled** | — |
| **`performance_targets`** | **unfilled** | — |
| **`primary_goal`** | **unfilled** | — |

`round=23, max_rounds=12`——实际跑了 23 轮、超出系统预设 11 轮，但仍有 3 个核心槽位空着。

### 2.3 用户参与质量样本（最后 5 条 user 消息）

| 消息序号 | 内容（截取） |
| --- | --- |
| 97 | 硬性上限（简单可靠）— 设置硬性限制：表达式最大长度 200 字符、嵌套深度 ≤ 10 层、数值范围 ±10³⁰⁸ 内 |
| 92 | 请以风险分析师的角度审视当前需求，找出最高影响的一个技术或安全风险 |
| 87 | **你决定 — 由你根据 macOS 原生行为确定** |
| 82 | **你决定 — 由你根据 macOS 原生行为确定** |
| 79 | 清空，开始新表达式 — 开始全新表达式：清空上方表达式行，下方显示 `5`，准备输入新表达式 |

最后 5 条里有 2 条是「你决定」——用户在第 80+ 轮已倦怠，授权 agent 自行决定。Preflight 的 compaction_summary 末尾写明：
> **第 23 轮被打断，风险审查后尚未回答**

即用户在第 23 轮直接走流程到签约，跳过了 agent 还想问的问题。

### 2.4 残留未澄清问题（compaction_summary 列出）

- 从历史记录选择存入 Memory 的行为：替换还是累加？
- 基础 ↔ 科学视图切换时窗口尺寸行为：是否自适应？
- 科学运算极端精度处理：`sin(π)` 显示 `0` 还是 `1.22e-16`？
- SwiftData Schema 升级/...（截断）

---

## 3. 阶段 2：Contract & Planning

### 3.1 Contract

| 字段 | 值 |
| --- | --- |
| `id` | `bf1cb7cd-38f6-4c65-90d0-3ceedbd33d5c` |
| `status` | `signed` |
| `signed_at` | 2026-05-12 16:28:01 |
| `budget_usd` / `quality_threshold` / `max_duration_hours` | **全部 NULL** |

### 3.2 Contract Items 来源分布

| Section | Count | by user | by agent |
| --- | --- | --- | --- |
| scope | 10 | **0** | 10 |
| constraints | 2 | **0** | 2 |
| exclusions | 2 | **0** | 2 |
| assumptions | 3 | **0** | 3 |
| **总计** | **17** | **0** | **17** |

contract 完全由 agent 生成，用户没主动添加任何条款。mission `directives` 字段里的"**自动化测试可以简化，快速完成任务优先**"**不在任何 contract item 内**——但这不是"信息流断裂"，而是因为 directive 是后期（DAG 执行阶段）通过 FM-06 Runtime Intervention 写入的，contract 签约时根本还不存在（详见 §3.4）。

### 3.3 Planner Sessions

| # | id | kind | status | steps | tokens | duration | 备注 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | `66920b61` | preflight | **failed** | 15 | 2,184 | 3 min | `Planner timed out after 180s` |
| 2 | `045dc84f` | preflight | completed | 58 | **347,952** | 8 min | **孤儿——结果未被使用** |
| 3 | `f3c70862` | preflight | completed | 40 | 171,888 | 6 min | **被采纳→ 触发 contract 签约** |

两件异常：
1. 第 1 次因 180s 硬超时失败
2. 第 2 次（08:21:59）和第 3 次（08:22:25）几乎同时启动（间隔 26 秒），都跑到 completed，但只有第 3 次的结果触发签约。第 2 次跑完后的产物没有被使用——纯浪费 348K tokens、58 步、8 分钟

`kind` 字段值为 `preflight` 但 schema 允许 `planner`——所有 planner session 都被记成 `preflight`，疑似字段语义错位。

### 3.4 FM-06 Runtime Intervention 实际发生情况

`missions.directives` 字段当前值为单条字符串 `"自动化测试可以简化，快速完成任务优先"`（无换行分隔符，说明只发生过 1 次写入）。

写入路径只能是 `inject_mission_note` 命令（`commands/agent.rs:687`），该命令有硬前置：**mission 必须至少有 1 个 status='running' 的 agent**，否则返回 `"No running agents in this mission"`。因此 directive 写入时机必然落在 DAG 执行阶段（5/18 11:34 之后），不可能在 preflight / contract / planning 阶段。

`inject_mission_note` 的双通道行为：

| 数据 | 落点 | 受众 |
| --- | --- | --- |
| `agent_notes` 表 INSERT 一行 | 每个当前 running agent 各一条（绑定 `agent_id`）| **当下正在跑的** agent，下一轮 LLM 调用前作为 `note_applied` system message 注入 |
| `missions.directives` append 一行（`||char(10)||`）| Mission 级，持久化 | **后续才启动**的新 agent（含 retry），scheduler `agent/scheduler.rs:843-852` 拼到 task_desc 末尾，标题 `[Standing Mission Directives — you MUST follow these]` |

本次实际数据状态：

- `agent_notes` 表：**空**（推测：所有原始 note 因 `agent_id REFERENCES agents(id) ON DELETE CASCADE`，在 retry 删旧 agent 时被级联清除）
- `agent_events` 表：跨整个 mission **无 `note_applied` 事件残留**（同上原因，agent 删则 events 删）
- `missions.directives`：单条字符串持久保留（在 missions 表上，不随 agent 删除而消失）

因此**直接证据已丢失**，但根据 inject 命令的前置约束可推断：用户是在某次 tester 运行期间观察到 agent 过度严苛后下发的（最可能是 tester#1 `ab2ba0aa`）。

对后续 agent 的影响（**可代码验证**）：

- tester#2 (`8e4a8fc5`) 启动时，scheduler 读取 `missions.directives` 并拼接到 prompt
- tester#3 (`2b8c40ee`) 同上
- 但两次 tester 重启后的实际行为仍是 fix-test 循环、纠结 60+ 用例覆盖，**directive 在 prompt 里但未真正影响行为**

⚠️ 旁路问题：`commands/agent.rs:687` 用 `with_conn` 而非 `unchecked_transaction`，`append_mission_directive` 和 `insert_note_for_mission` 之间非事务，半写状态可能留下"directives 有但 agent_notes 空"的脏数据（违反 `retryable-flow.mdc` 规则 2）。本次因 CASCADE 清理导致难以区分是脏数据还是正常状态。

---

## 4. 阶段 3：DAG 执行（Implementer + Integrator + Architect）

### 4.1 任务编排

| Task ID | Role | Status | Title (截取) | Steps | Tokens | 用时 |
| --- | --- | --- | --- | --- | --- | --- |
| `ba6b3b57` | architect | completed | Design app architecture and module contracts | 21 | 240K | 7 min |
| `c1b3912a` | implementer | completed | Implement calculator engine with scientific functions | 46 | 1.19M | 25 min |
| `a47f4d45` | implementer | completed | Implement SwiftData persistence for history and memory | 20 | 205K | 4 min |
| `511bb97f` | implementer | completed | Build basic calculator UI with keyboard support | 32 | 714K | 13 min |
| `fb0cccaa` | implementer | completed | Implement scientific mode UI and view switching | 31 | 941K | 14 min |
| `fb3f1c1d` | implementer | completed | Build paper-tape history panel with memory integration | 50 | 1.55M | 16 min |
| `0b06323b` | implementer | completed | Implement M+/M-/MR/MC memory operations UI | 70 | 2.50M | 19 min |
| `1f8b2389` | implementer | completed | Add URL Scheme handler and float-on-top window | 51 | 1.53M | 17 min |
| `b2928237` | integrator | completed | Assemble app entry point, menus, and wire all modules | 53 | 1.90M | 12 min |
| `22d4be5a` | **tester** | **failed→cancelled** | Write comprehensive test suite for all modules | 80 / 58 / 24 (三次) | 3.24M+2.29M+0.72M | 详见 §6 |

`complexity` 字段所有任务均为 `medium`——没有差异化标签，全部用统一预算。

### 4.2 实际并发情况

5/18 03:34 (UTC) 起，scheduler 同时调度多个 implementer：

- **Wave 1**: c1b3912a (engine) + a47f4d45 (persist) —— 同时启动
- **Wave 2**: 511bb97f (basic UI) 在 engine 完成后启动；继而 fb0cccaa / fb3f1c1d / 0b06323b / 1f8b2389 在 04:13 几乎同时开始 (basic UI 完成后)
- **Wave 3**: b2928237 (integrator) 在 04:32 启动（最后一个 implementer 完成后）

Implementer 阶段最大并发 4。Integrator 串行。

### 4.3 各 Agent 事件分布

| Agent | Events | Errors | Tool_use | Compact | Tool_summary |
| --- | --- | --- | --- | --- | --- |
| dd68c400 (architect) | 97 | 5 | 24 | 0 | 0 |
| 58c0d1ba (engine) | 200 | 5 | 49 | 0 | 2 |
| d6816c7d (persist) | 105 | 2 | 27 | 0 | 2 |
| dfb4ecad (basic UI) | 151 | 1 | 38 | 0 | 3 |
| e52a0f77 (sci UI) | 187 | 1 | 55 | 0 | 8 |
| 38498ac3 (history) | 237 | 9 | 61 | 0 | 7 |
| f2035a1a (memory) | 327 | **14** | 85 | 0 | 8 |
| aa29d3f0 (URL) | 245 | 7 | 63 | 0 | 6 |
| 8f493d02 (integrator) | 281 | 4 | 76 | 0 | **14** |
| 2b8c40ee (tester#3) | 166 | 4 | 54 | 0 | 6 |

观察：
- **`compact=0` 跨所有 agent**：context compaction 从未触发
- **`tool_summary` 集中在后期 agent**（integrator 14, history 7, memory 8）：单步 tool 输出超阈值被自动摘要
- **没有一个 implementer/architect 是 0 errors** —— 即使「正常完成」的任务都伴随多个错误事件

### 4.4 错误类型分布（跨所有 mission agent）

| 错误类型 | 次数 | 含义 |
| --- | --- | --- |
| `shell_error` | 37 | shell_exec 命令 exit != 0 |
| `io_error` | 5 | 工具底层 IO 失败（如 `failed to spawn rg: No such file or directory`）|
| `parameter_error` | 2 | 工具入参校验失败 |
| `missing_or_invalid_arguments` | 2 | LLM 输出的 tool args 解析失败（JSON 不完整）|
| `edit_no_match` | 2 | `edit_file` 的 `old_string` 在目标文件找不到 |
| `artifact_error` | 2 | publish_artifact 失败 |
| `unknown_tool` | 1 | LLM 调用了未注册的 tool 名 |
| `approval_expired` | 1 | shell_exec 需 approval 超时未确认（中午无人）|

### 4.5 Git 物理产物

| 分支 | Commits ahead of main |
| --- | --- |
| `agent/58c0d1ba` (engine) | 4 |
| `agent/d6816c7d` (persist) | 2 |
| `agent/8f493d02` (integrator) | 22 |
| ... | ... |

`main` 分支始终是 `3887920 Initial commit`——**整个 mission 没有任何代码被 merge 回 main**。失败的 mission 阻塞了最后的 merge 步骤，全部产物滞留在 agent/* 和 task-base/* 分支上。

物理磁盘占用（worktree 数 = 30，prunable = 15）：

```
1.0 GB   /macos-calculator-with-scientific-mode-an-26d64c5d/
158 MB   .worktrees/ab2ba0aa (tester#1, DB record 已删)
159 MB   .worktrees/8e4a8fc5 (tester#2)
90 MB    .worktrees/8f493d02 (integrator)
87 MB    .worktrees/aa29d3f0 (URL)
84 MB    .worktrees/2b8c40ee (tester#3)
...      (10+ 个 prunable 残留)
```

---

## 5. 阶段 4：Evaluator（数据暴露的结构性问题）

`evaluator_reviews` 表里每个 implementer agent 被独立打分。打分内容显示：**evaluator 在评分单个 implementer 时，对照的是整个 mission 的需求**，而非该 implementer 自己的 task 范围。

| Agent | Task 范围 | Score | Summary（摘要） |
| --- | --- | --- | --- |
| 58c0d1ba | calculator engine | **9.0** | "Engine code is well-structured... Minor style issues" |
| d6816c7d | persistence | 8.0 | "PersistenceController silently swallows save errors" |
| **dfb4ecad** | **basic UI** | **4.0** | "Memory operations and SwiftData persistence are entirely absent, history panel, URL scheme handling, float-on-top, and scientific keyboard have not been implemented" |
| **38498ac3** | **history panel** | **6.0** | "M+/M-/MR/MC memory operations... missing... scientific mode... is entirely missing" |
| **aa29d3f0** | **URL handler** | **5.0** | "scientific mode, history panel, SwiftData persistence... not implemented" |

basic UI / history panel / URL handler 三个 agent 被指责"没实现整个计算器"——但它们的 task 本来就是分工的小块。**评分明显错位**，且这些低分**没有阻塞任务流转**（agent 都顺利 completed），评分行为没有任何下游 consumer。

---

## 6. 阶段 5：Tester 三次失败详情

### 6.1 时间线汇总

| # | Agent ID | 启动 | 结束 | 步数 | Tokens | 失败原因 |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | `ab2ba0aa` | 5/18 12:?? | 5/18 13:10 | 80 | 3.24M | `max_steps: 80 steps exhausted without task_complete`，DB 记录已被 scheduler retry 时删掉，仅留 worktree |
| 2 | `8e4a8fc5` | 5/18 14:20:12 | 5/18 14:50:12 | 58 | 2.29M | `timeout: wall_clock 1800s exceeded` |
| 3 | `2b8c40ee` | 5/18 16:07:51 | 5/18 16:26:59 | 24 | 0.72M | `cancelled: user stop`（用户主动取消）|

### 6.2 tester#1 关键现场（来自 IDE terminal 日志）

- 反复修 Swift 测试可见性（"`internal` protection level"），改 source 把 `func switchToScientific()` 改 public
- 撞 `factorialOf(0)` / `factorialOf(1)` 的 `2...intN` ClosedRange fatalError
- 撞 `URLHandlerTests`：parser 设计宽容 `2++3`→`2+3`，测试期望"语法错误"
- fix → test → fix → test 循环最后跑爆 step 上限

### 6.3 tester#2 关键现场（agent_events 表）

| Step | 现象 |
| --- | --- |
| 3 | `tool_summary`: 27,444 chars → 111 chars（压到 0.4%）|
| 4 | `tool_summary`: 25,940 chars → 249 chars（压到 1.0%）|
| 6 | `system_hint`: "You have spent 5 consecutive steps only reading / searching files without making any change" |
| 7 | `tool_summary`: 8,476 chars → 270 chars |
| 14 | `error`: `missing_or_invalid_arguments` —— write_file 5KB 内容 JSON escape EOF |
| 21 | `error`: `approval_expired` —— shell_exec 需要 approval 但中午无人确认 |
| 40 | `error`: `io_error: failed to spawn rg: No such file or directory` |
| 55-56 | 读 ExpressionParser.swift 看 factorial → 改测试加注释 `// Factorial 1! and 0! are skipped — engine has range bug for n < 2` |
| 57 | shell_exec 跑 `swift test`（完整重新编译）|
| 58 | 评审测试结果时，wall_clock 1800s 切断 |

注意 step 56 的注释——agent 已经识别出 implementer 的 bug，但**没有反馈通道** ("raise bug to upstream task")，只能选择改测试绕开。

### 6.4 tester#3 关键现场

| Step | 现象 |
| --- | --- |
| 1 | 并发 read 6 个 source 文件（engine/parser/viewmodel/persist/URL handler）|
| 7 | `io_error: failed to spawn rg`（**与 tester#2 完全相同**，agent 没学到经验）|
| 20 | `edit_no_match`：尝试改 `HistoryEntry` 加 `public` 修饰，old_string 找不到 |
| 22-23 | write_file 改 `HistoryEntry` 和 `MemoryStore` 加 public |
| 24 | 用户取消，agent 还在准备阶段（让 SwiftData 模型对测试可见），真正写测试都没开始 |

tester#3 走的路径跟 tester#1/#2 高度相似：都先撞 `internal` 可见性问题，先改 source 加 public——但**改的不是同一个 worktree**，每次 retry 都从零重学。

### 6.5 Retry 策略观察

`restart_mission` 在 mode=`failed_only` 时：

1. `delete_agents_for_tasks([22d4be5a])` —— 旧 agent DB 记录被删（worktree 文件保留）
2. `reset_failed_tasks` —— task status `failed` → `pending`
3. mission status `failed` → `planned`
4. auto_start → 新 agent_id + 新 worktree（从重建后的 `task-base/22d4be5a` 派生）

特点：**重建任务时不携带任何"前一次失败的经验"**——新 agent 是 cold start，prompt 完全相同，只有 base 代码可能因为外部修改而改变。

---

## 7. 阶段 6：开发者外部修复（不在 mission 系统内）

时间 5/18 15:50 ~ 16:00（在 tester#2 失败后 / tester#3 启动前），通过 Cursor IDE 在 worktree 内手工：

| 动作 | 落点 | 结果 |
| --- | --- | --- |
| 修 `factorialOf` `n<2` 早退 | tester#2 worktree（`agent/8e4a8fc5`）的 working tree | swift test 154/154 全过 |
| 调整 4 个测试期望对齐 implementer 设计 | 同上 | 同上 |
| factorial 修复 commit 到 `agent/58c0d1ba` | commit `014ca58` | 进入 source-of-truth |
| factorial 修复 commit 到 `agent/8f493d02` | commit `5c40388` | 防 theirs 策略下被旧版覆盖 |
| git merge-tree 模拟 `prepare_task_base` 验证 | 临时 clone | factorial 修复在合并后保留 |

模拟验证后用户在 UI 上触发 restart_mission（mode=failed_only, auto_start=true）→ tester#3 启动 → 19 分钟后用户取消。

外部修复**没有进入** mission 的任何状态记录（agent_notes、decision_log、artifacts 都没条目）——对系统而言这些 commit 是"凭空冒出"的。

---

## 8. 资源消耗汇总

### 8.1 Tokens

| 阶段 | Input | Output | 累计 |
| --- | --- | --- | --- |
| Preflight (1 session) | 361,060 | 26,516 | 387,576 |
| Planner (3 sessions) | — | — | ~520,000 (sum of total_tokens) |
| DAG 9 个正常 agent | — | — | ~10,775,000 (sum of tokens_used) |
| Tester 3 次尝试 | — | — | ~6,247,000 (3.24M + 2.29M + 0.72M) |
| **总计** | | | **~17.9M tokens** |

`missions.total_cost_usd = 0.0`（字段未被任何流程更新，存在 schema 但无写入路径）。`agents.cost_usd` 也未实际填充。

### 8.2 时间

- 实际 wall-clock：7 天（含 5/12-5/16 用户未操作的 4 天 + 5/16 architect 完成到 5/18 implementer 启动的 42 小时）
- 自动执行 wall-clock：≈ 3 小时（5/18 11:34 → 14:50 二次失败）
- 用户有效参与时间：preflight 23 轮对话 + 多次 UI 操作（粗估 1-2 小时）

### 8.3 磁盘

- workspace 根目录：**1.0 GB**
- worktree 数量：12 个（其中 prunable 15 个 ref 残留）
- 单 worktree 平均：~80-150 MB（主要是 `.build/` Swift 编译产物）

---

## 9. 关键统计数字一览

| 维度 | 数字 |
| --- | --- |
| Mission 全流程总时长 (wall) | 7 天 |
| 实际并发执行时长 | ~3 小时 |
| Preflight 轮次 / 上限 | **23 / 12** |
| Preflight convergence | 0.64 (未达 ready_to_sign) |
| Preflight unfilled slots | 3 / 11 |
| Contract items 由用户撰写 | **0 / 17** |
| Runtime intervention 下发次数 | **1**（运行时通过 FM-06 inject 进 directives）|
| Directive 传递到后续 retry 的 tester prompt | **已传递**（scheduler 拼接确认）|
| 但 agent 实际遵守该 directive | **否**（tester#2/#3 仍按"全覆盖"模式跑）|
| Planner 启动次数 | 3 (1 timeout + 2 completed) |
| Planner 孤儿运行浪费 | 1 次 / 348K tokens / 8 分钟 |
| 正常完成的 Agent | 9 |
| Tester 启动次数 / 失败次数 | 3 / 3 |
| Evaluator reviews | 5 条全部范围错位 |
| 累计 tokens | ~17.9M |
| Mission cost_usd 字段 | 0.0（未填充）|
| Git commits 入 main | **0** |
| 物理 worktree 数 | 12 (15 prunable refs) |
| 磁盘占用 | 1.0 GB |

---

## 附录 A：本次复盘使用的 SQL 查询

为方便后续复用，归档关键查询：

```sql
-- mission 元数据
SELECT id, title, description, directives, status, total_cost_usd,
       created_at, updated_at, repo_path, repo_origin, merge_strategy
FROM missions WHERE id='<mission_id>';

-- preflight 概况
SELECT id, mode, phase, convergence_score,
       cumulative_input_tokens, cumulative_output_tokens,
       json_array_length(messages) AS msg_count,
       compaction_failures, belief_state
FROM preflight_sessions WHERE mission_id='<mission_id>';

-- contract items 来源
SELECT section, COUNT(*), SUM(source='user'), SUM(source='agent')
FROM contract_items WHERE contract_id='<contract_id>' GROUP BY section;

-- planner 历史
SELECT id, kind, status, total_steps, total_tokens,
       created_at, completed_at, error_message
FROM planner_sessions WHERE mission_id='<mission_id>' ORDER BY created_at;

-- 任务全表
SELECT id, role, status, complexity, title,
       guardrail_retry_count, last_error
FROM tasks WHERE mission_id='<mission_id>' ORDER BY created_at;

-- agent 全表
SELECT a.id, a.task_id, a.status, a.current_step, a.tokens_used,
       a.created_at, a.updated_at
FROM agents a JOIN tasks t ON a.task_id=t.id
WHERE t.mission_id='<mission_id>' ORDER BY a.created_at;

-- 错误类型分布
SELECT json_extract(content,'$.error') AS err_type, COUNT(*) AS cnt
FROM agent_events e JOIN agents a ON e.agent_id=a.id
JOIN tasks t ON a.task_id=t.id
WHERE t.mission_id='<mission_id>' AND e.kind='error' AND json_valid(content)
GROUP BY err_type ORDER BY cnt DESC;

-- evaluator reviews
SELECT agent_id, overall_score, summary
FROM evaluator_reviews
WHERE agent_id IN (SELECT a.id FROM agents a JOIN tasks t ON a.task_id=t.id
                   WHERE t.mission_id='<mission_id>');
```

## 附录 B：相关代码定位

| 现象 | 代码 |
| --- | --- |
| Preflight session 推进 | `src-tauri/src/agent/preflight*.rs` |
| Belief state 槽位定义 | `src-tauri/src/agent/preflight*.rs` (slot 字典常量) |
| Planner 超时 | `src-tauri/src/agent/planner_engine.rs` |
| Restart mission | `src-tauri/src/commands/mission.rs:986` `restart_mission` |
| Task base 构建（重启时重建）| `src-tauri/src/agent/scheduler.rs:1047` `build_task_base` + `src-tauri/src/git/worktree.rs:146` `prepare_task_base` |
| Agent retry / max_steps / wall_clock | `src-tauri/src/agent/scheduler.rs` (AgentRunOptions / default budgets) |
| Tool summarization | `src-tauri/src/tools/executor.rs` (tool_summary 触发阈值) |
| Evaluator | `src-tauri/src/agent/evaluator*.rs` |
| FM-06 Runtime Intervention 写入 | `src-tauri/src/commands/agent.rs:662 inject_mission_note` → `queries::append_mission_directive` (`db/queries.rs:982`) + `insert_note_for_mission` |
| Directives 注入 retry 后的 agent prompt | `src-tauri/src/agent/scheduler.rs:843` (spawn 时读取 + 拼接 `[Standing Mission Directives]` 段) |
| `agent_notes` 级联清除（retry 时丢失历史 note） | schema：`agent_notes.agent_id REFERENCES agents(id) ON DELETE CASCADE` |
