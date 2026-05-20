# Miragenty 全流程优化提案

> **基于**: [`2026-05-18-macos-calculator-postmortem.md`](./2026-05-18-macos-calculator-postmortem.md)
> **日期**: 2026-05-18
> **范围**: Preflight → Contract → Planning → DAG Execution → Tester / Evaluator → Restart 全链路

本文档基于一次完整失败案例提炼，**所有提案都引用 postmortem 中的具体数据**作为依据，不做无凭据的"重构万物"。每条提案标注：影响维度、优先级、改动范围、验收信号、可能的负作用。

---

## 0. 评估框架

5 个评估维度（一条优化可能影响多维）：

| 代号 | 维度 | 这次 case 的具体表现 |
| --- | --- | --- |
| **S** | **Success rate** 流程能否成功 | 9/10 task completed，但 tester 卡死，最终 mission failed |
| **C** | **Convergence speed** 收敛速度 | preflight 23 轮没收敛，tester 重试 3 次无进展 |
| **$** | **Cost / Token / Disk** 资源 | 17.9M tokens，348K tokens 孤儿浪费，1.0 GB worktree |
| **UX** | **User experience** 用户体验 | 用户参与从 21→「你决定」→打断签约→中午白屏→最终 cancel |
| **O** | **Observability** 可观测/可调试 | cost_usd 字段全 0、evaluator 评分错位无人发现、retry 不带经验 |

优先级：
- **P0** —— 这次 case 直接造成失败的根因，**不修下次还会发生**
- **P1** —— 高 ROI、风险可控，下个 sprint 应该做
- **P2** —— 架构级改造，需 RFC + 多 sprint 投入
- **NO-GO** —— 看似合理但权衡后不推荐做的方向

---

## 1. 关键洞察（meta level）

从这一个 case 已经能抽出来的横向规律：

### 洞察 1：运行时干预（FM-06）的"有效性衰减"

`missions.directives = "自动化测试可以简化，快速完成任务优先"` —— 这条不是 mission 创建时的设定，是用户在某次 tester 运行期间通过 UI 调 `inject_mission_note` 下发的**运行时干预**（详见 postmortem §3.4）。

这条干预的传递路径**机械上是完整的**：

| 通道 | 实现位置 | 本次实际 |
| --- | --- | --- |
| 立即注入当前 running agent | `agent_notes` 表 + 下一轮 LLM `note_applied` system message | 已发生（但 `ON DELETE CASCADE` 让历史 note 在 retry 后物理消失）|
| 持久化给后续 retry / 新 agent | `missions.directives` append → scheduler.rs:843 拼到 task_desc 末尾，加 `[Standing Mission Directives — you MUST follow these]` 标题 | 已发生（tester#2/#3 的 prompt 里都有这段）|

但**行为上完全没起作用**：tester#2/#3 重启后仍按"全模块全覆盖"模式跑，反复在 fix-test 循环里。这条 directive 在 prompt 里却没有被 agent 真正贯彻。原因有三层：

1. **沉没成本**：tester#1 已经在 worktree 里写了大量测试文件，retry 的新 worktree 是从 task-base 重建的（不继承 tester#1 的 working tree），但 task description 还要求"comprehensive test suite"——agent 看到一个"必须达成"的任务目标 + 一条"可以简化"的建议时，**结构性偏向前者**
2. **prompt 权重不足**：directives 只是拼到 task_desc 末尾的一段文字，没有 "STOP, drop current todo, restart with simplified scope" 这种带操作语义的强约束
3. **干预与任务定义割裂**：原始 task 是 planner 在签约后写的"60+ 用例"，directive 是后期人工加的"简化"；两者矛盾时，agent 没有机制裁决谁优先

**这是这次失败 retry 阶段的真正结构性问题**——不是信息没传到，是传到了但没"打断与重定向"能力。

### 洞察 2：Cold-start retry 模式天然反收敛

`restart_mission(failed_only)` 删旧 agent → 创建新 agent → 同 prompt 从零开始。tester 三次启动都先做相同事：读 6 个 source、撞 `internal` 可见性、改成 public、撞 factorial、修测试……前一次跑了 80 步学到的"这个 codebase 的坑"在第二次完全失忆。**重试就是同费用买一次相同的失败**。

### 洞察 3：评估系统失灵且无人察觉

`evaluator_reviews` 5 条记录里 3 条把"单模块 implementer"按"整个 mission"打分，basic UI agent 拿 4.0 因为"没实现 scientific mode"。**这种明显错位至少存在了一整轮 mission**，没有 UI 告警、没有阻塞流程、没有任何下游消费——评分行为完全是"空跑"。这意味着 evaluator 这个系统**当前不在 critical path 上**，可能整个特性都没真正激活。

### 洞察 4：失败的代价是不对称的

正常 9 个 agent 跑完平均 16 分钟，failed agent 的 wall_clock 是硬上限 30 分钟。Tester 失败 2 次直接吃掉 60 分钟 + 5.5M tokens。**失败成本是成功的 ~2 倍**，且没有提前止损机制（前 10 分钟看 trajectory 就知道在循环但没人切断）。

### 洞察 5：物理资源治理缺位

12 个 worktree × ~80MB = 1 GB，其中 15 个 prunable ref 残留。`agents` 表被 retry 清掉后，磁盘上的 worktree 变成孤儿。**没有自动清理**，也没有 UI 显示总占用。重跑 5 次后 5 GB 起步。

---

## 2. P0 优化（必修）

### P0-1 Runtime Intervention 升级：从"加一句提示"到"打断与重定向"
**维度**: S / C / UX
**依据**: 用户发"自动化测试可以简化"后，directive 在 tester#2/#3 的 prompt 里都被拼接，但 agent 仍按"全覆盖"跑（详见 §1 洞察 1）；现有 `inject_mission_note` 只塞一条 system note，对已陷入 fix-test loop 的 agent 影响微弱

**现状**：FM-06 Runtime Intervention (`commands/agent.rs:662`) 单一行为是 INSERT 一条 `agent_notes` + append `missions.directives`。对当下 running agent 只是"下轮多一段 system message"，对未来 retry 只是"prompt 末尾多一段文字"。缺少"让 agent 真的改方向"的执行语义。

**提案**：

**P0-1A：给 inject 命令引入"干预模式"**（核心改造，~3 天）

UI 上下发干预时让用户选 mode，对应不同的 agent 端行为：

| Mode | 对 running agent 的效果 | 适用场景 |
| --- | --- | --- |
| `hint` | 当前行为（追加一条 system note）| 提示性补充，不要求改方向 |
| `redirect` | 注入 note + 强制清空当前 `todo_update` + 要求下一步必须 `todo_update` 重写计划 | "你正在跑偏，请按这条重新规划" |
| `abort_with_partial` | 注入 note + 立即触发 `task_complete`，把当前 working tree 作为 partial 产出 | "够了，别再死磕，把已有的当结果" |

实现要点：
- `InjectAgentNoteRequest` / `InjectMissionNoteRequest` 加 `mode: "hint"|"redirect"|"abort_with_partial"` 字段（默认 `hint` 向后兼容）
- `redirect` 模式：scheduler 在 agent 主循环检测到 note 后，下一步前主动注入额外 system prompt `"Your previous todo list has been cleared. You MUST call todo_update first with a revised plan that incorporates this directive."`
- `abort_with_partial`：让 agent engine 收到一个特殊 control flag，在下次 tool-loop 转角直接 `task_complete(partial=true, reason="user_aborted_with_directive")`

**P0-1B：directives 在 retry prompt 里改成带"前因"的强约束**（小改，~0.5 天）

scheduler.rs:843 当前拼接是干巴巴的 `[Standing Mission Directives — you MUST follow these]\n{directives}`。改成：

```
[Standing Mission Directives — your predecessor on this task was interrupted by the user to enforce these]

The following directives were issued during prior execution attempts and represent
the user's resolved priorities. They OVERRIDE any conflict with the task description
above. If a directive says "simplify X", you MUST reduce scope on X even if the
task description asks for completeness.

{directives}
```

让 agent 明确知道：这不是建议，是用户中断前任后立下的优先级裁决。

**P0-1C：`inject_mission_note` 改用 `unchecked_transaction`**（顺手 fix，~10 分钟）

当前 `commands/agent.rs:687` 用 `with_conn` 包多条 SQL，违反 `retryable-flow.mdc` 规则 2。半写状态会留下"directives 有但 agent_notes 空"的脏数据。改用事务一行解决。

**P0-1D：retry 时把已死亡 agent 的 inherited notes 复制给新 agent**（中改，~1 天）

当前 `agent_notes.agent_id REFERENCES agents(id) ON DELETE CASCADE`：retry 删旧 agent → 老 note 物理消失。给 `agent_notes` 加 `inherited_from_agent_id` 字段，retry 时：
- 把适用于该 mission 的 note（status='applied' 的）复制给新 agent，标记 `inherited_from_agent_id=<old_id>`，置 status='queued'
- 新 agent 开跑前自动注入这些 inherited notes（带前缀 "[Inherited from your predecessor]"），让它继承前任的用户干预上下文

**验收**：
- E2E：mission 跑到一半发 `redirect` mode 干预 → 下一步 agent 的 events 应该包含一个 `todo_update`，且新 todo 与原 todo 在 hash 上不同
- E2E：retry 后 tester 第一步的 prompt 必须包含 inherited notes 段
- 单测：
  - `test_inject_mission_note_uses_transaction_atomicity`（P0-1C）
  - `test_redirect_mode_forces_todo_rewrite_in_next_step`（P0-1A）
  - `test_retry_inherits_applied_notes_from_predecessor`（P0-1D）
  - `test_directives_prompt_includes_override_language`（P0-1B）

**落点**：
- `src-tauri/src/commands/agent.rs:610-720`（inject 命令加 mode 字段 + 改事务）
- `src-tauri/src/agent/scheduler.rs:843`（拼接文案 + retry 时复制 notes）
- `src-tauri/src/agent/engine.rs`（识别 `redirect` / `abort_with_partial` control flag）
- `src-tauri/src/db/migrations.rs`（`agent_notes` 加 `inherited_from_agent_id` 列；`agent_notes`/`missions.directives` schema 不动）
- `src/components/agent/InjectNoteDialog.tsx`（UI 三态选择，默认 hint）

**工作量**：合计 ~5 天（A=3 + B=0.5 + C=0.1 + D=1，留 0.4 整合）

**风险**：
- `redirect` 模式可能让 agent 反复重写 todo 而不前进 → 限制：单 agent 同 mission 内 `redirect` 干预 ≤ 3 次，超过强制降级为 `hint`
- `abort_with_partial` 误用让用户丢工作 → UI 上需二次确认 + 自动 git commit 当前 working tree 到 `agent/<id>-aborted` 分支兜底

---

### P0-2 Implementer 留 bug → Tester 必须能反馈，禁止默默改测试绕开
**维度**: S / C
**依据**: tester#2 step 56 注释 `// Factorial 1! and 0! are skipped — engine has range bug for n < 2` —— agent 识别出 bug 但没有反馈通道，只能 cover-up

**现状**：tester role 只能 read/write/edit 自己 task scope 的文件（测试文件），但实测 tester 经常需要改 source（加 `public` 修饰、绕开 bug），没有"raise bug to upstream"的工具。

**提案**：
1. 给 tester role 加一个工具 `report_upstream_defect(file:str, line:int?, description:str, severity:"blocker|major|minor")`
2. 调用此工具会：
   - 在 `tasks` 表为有问题的 upstream task 创建一个 child fixer task（role=fixer）
   - 把 tester 自己标记为 `waiting_on_dependency`，不消耗 wall_clock
   - fixer task 复用 upstream implementer 的 prompt + bug description，单次预算 max_steps=20 / wall_clock=600s
3. fixer 完成后自动 unblock tester；fixer 失败则 tester 也标 failed（避免无限套娃）

**验收**：
- 用例：人为在 engine 注入 factorial bug，tester 应触发 fixer 任务，而非改测试跳过
- 测试：`test_tester_raises_defect_creates_fixer_task`

**落点**：`src-tauri/src/tools/definitions.rs` (新 tool) + `src-tauri/src/agent/scheduler.rs` (fixer task 调度)

**工作量**：3-5 天

**风险**：滥用风险（tester 把任何 fail 都 raise 成 bug） → 加 `severity` + 单 task fixer 触发上限 (默认 2 次)

---

### P0-3 Retry 必须携带"前次失败经验"
**维度**: S / C / $
**依据**: tester 三次启动每次都重复"读 6 个 source → 撞 internal → 加 public" 同一套动作

**现状**：`restart_mission` 的 `failed_only` 模式直接 `delete_agents_for_tasks` 删除旧 agent 记录，新 agent 完全 cold start。

**提案**：
1. **保留**旧 agent 记录（status 改为 `superseded`），不删
2. 新 agent 启动时，scheduler 把同 task 历史 agent 的 **lessons learned** 注入 system prompt：
   - 上次失败原因（`last_error`）
   - 上次最后 3 步的 tool_use（让新 agent 知道前任在做什么没做完）
   - 自动从旧 agent 的 `compaction_summary` / `agent_notes` 提炼
3. UI 上 task 详情显示「历史尝试」时间线，每次失败一条

**验收**：
- 用例：第 2 次 retry 的 prompt token 数比第 1 次至少多 500（携带了 lessons），但 step 1 不应再读已读过的同名文件
- 测试：`test_retry_inherits_lessons_from_predecessor_agent`

**落点**：`src-tauri/src/commands/mission.rs` (restart_mission 改 superseded 不删) + `src-tauri/src/agent/scheduler.rs` (spawn agent prompt 构造)

**工作量**：2-3 天

**风险**：历史经验注入可能"误导"新 agent 重复错路 → 设计成"hint 而非约束"，并加 escape token "your predecessor failed because X; you may take a different path"

---

### P0-4 Wall-clock budget 必须按 role × complexity 差异化
**维度**: S / $
**依据**: 所有 task `complexity=medium`，tester 跟 implementer 同样 30 分钟 wall_clock；但 tester 要写 60+ 测试 + 多轮编译，每轮编译 1-2 min

**现状**：scheduler 用单一 default `wall_secs=1800`，不区分 role 或 task 体量。tester#2 跑 58 步只到 swift test 第二轮就超时。

**提案**：
1. 把 wall_clock budget 从全局常量提到 `tasks` 表字段 `wall_clock_secs`（迁移加列，默认仍 1800）
2. Planner 生成 task 时按 role 写默认：
   - architect / persistence-only implementer: 1200s
   - implementer (含 UI): 1800s
   - integrator: 2400s
   - tester (multi-module): **3600s**（× 2）
   - tester (single-module，配合 P1-3 拆分): 1800s
3. UI 上 task 详情可手动调整 wall_clock，重启 task 时生效

**验收**：
- 实测：把 tester#2 wall_clock 改 3600s 跑同一份 prompt，应能跑完 swift test 完整 1-2 轮 fix
- 测试：`test_tester_default_wall_clock_doubles_implementer`

**落点**：`src-tauri/src/db/migrations.rs` (加列) + `src-tauri/src/agent/scheduler.rs` (读字段) + `src-tauri/src/agent/planner_engine.rs` (按 role 默认)

**工作量**：1-2 天

**风险**：长 wall_clock 让"卡死的 agent"占资源更久 → 配合 P1-1 (前期止损监控)

---

### P0-5 Evaluator 评分必须以 task scope 为准，不是 mission scope
**维度**: O / S
**依据**: `evaluator_reviews` 里 basic UI agent (dfb4ecad) 得 4.0 因为"没实现 scientific mode"——它本来就不该实现 scientific mode

**现状**：evaluator prompt 拿到的是整个 mission 的 contract，按整体打分单 agent。

**提案**：
1. Evaluator 启动时 prompt 输入限定为：该 agent 对应 task 的 `description` + `expected_output` + `file_scope_hints` + `produces_artifacts`
2. **不提供** mission 的 contract scope items（除非 task 显式 reference）
3. 评分语义清晰：是「这个 agent 是否满足它被分配的 task」，不是「这个 agent 是否实现了整个 mission」
4. UI 显示 evaluator score 时加 `(task scope)` 标识

**验收**：
- 重跑 evaluator 对 dfb4ecad (basic UI)，期望评分 ≥ 7.0（它干完了 basic UI 该干的事）
- 测试：`test_evaluator_uses_task_scope_not_mission_scope`

**落点**：`src-tauri/src/agent/evaluator.rs`

**工作量**：1 天

**风险**：评分变高之后是否让 review 价值降低？配合 P1-2 提升评分的下游消费

---

## 3. P1 优化（高 ROI）

### P1-1 Agent 早期"无效循环"检测，主动止损
**维度**: S / C / $
**依据**: tester#2 step 6 已触发 system_hint「5 步连续 read 没改动」，但只是 hint 没行动；继续跑了 52 步才超时

**现状**：`system_hint` 写到 events 表但 agent 自己决定是否理会，scheduler 不监控。

**提案**：
1. Scheduler 监控每个 agent 的"行为熵"指标：
   - 连续 N 步只 `read_file` / `search_files`（无 write/edit/shell_exec） → suspect loop
   - 连续 N 步 `tool_result` 内容相似度 > 0.9 → suspect duplicate work
   - 同一个 file 被 read > 5 次 → suspect 无法理解
2. 触发熔断时：
   - 第一次警告：注入 `system_hint` 提示 "you appear stuck on X, take a different approach"
   - 第二次警告：强制 status → `failed` (`stuck_loop_detected`)，避免烧完 wall_clock
3. 阈值可配置（mission 级或全局）

**验收**：
- 重跑 tester#2 prompt 的 trajectory，期望在 step ~15-20 而非 step 58 被切断
- 测试：`test_scheduler_detects_read_only_loop`

**落点**：`src-tauri/src/agent/scheduler.rs` (新 anti-loop 监控器) + `src-tauri/src/agent/engine.rs` (熔断接入)

**工作量**：3-5 天

**风险**：误判（agent 确实需要长时间读代码） → 阈值保守 + 触发后 agent 可申诉一次

---

### P1-2 Evaluator 评分接入 gating
**维度**: O / S
**依据**: 5 条 evaluator review score 4.0-9.0 跨度很大但所有 agent 都 completed，分数没消费

**现状**：评分写库，UI 显示，但**不影响 agent / task / mission 状态**。

**提案**：
1. mission `quality_threshold` 字段已经存在（schema 里有），但目前是 NULL。让 contract 必须设置（默认 7.0）
2. agent 完成且 evaluator score < threshold 时：
   - 自动创建一个 fixer task（同 P0-2 fixer），把 evaluator 的 issue annotations 作为 input
   - fixer 通过后 agent 才标 completed
3. UI 上区分 `completed (score=8.5)` vs `completed-after-fix (score=5.0→7.5)`

**验收**：
- 重跑此 mission，d6816c7d (persistence, score 8.0) 直接 completed；dfb4ecad (basic UI, 修正 P0-5 后) 应在 ≥ 7.0
- 测试：`test_low_score_triggers_fixer_before_complete`

**落点**：`src-tauri/src/agent/evaluator.rs` + `src-tauri/src/agent/scheduler.rs`

**工作量**：5-7 天（含 P0-5 前置）

**风险**：fixer 又失败的死循环 → fixer 最多 1 次，超后让 agent failed

---

### P1-3 Tester role 按 module 拆分
**维度**: S / C / $
**依据**: tester task description 要求"4 个测试文件、60+ 用例、跨 4 个 module"，明显是 mission spec 整体平移；DAG 内其他 task 都是单 module / 单功能

**现状**：planner 把所有"写测试"打包成一个 tester task。

**提案**：
1. Planner 生成 task 时，对每个 implementer task 自动生成一个对应的 tester sub-task：
   - `c1b3912a` engine implementer → tester task: "Write unit tests for `CalculatorEngine` (target: 20+ cases)"
   - `a47f4d45` persistence → tester task: "Write integration tests for `PersistenceController`"
   - 依此类推
2. 这些 tester task 可以并发跑（依赖各自的 implementer，互不依赖）
3. 各 tester task 单独的 wall_clock 1800s 足够

**验收**：
- 重跑此 mission，应生成 ~6 个 tester task 而非 1 个；总 wall_clock 占用反而降低（并发）
- 测试：`test_planner_generates_one_tester_per_implementer_when_directive_requires_tests`

**落点**：`src-tauri/src/agent/planner_engine.rs` / planner 的 task decomposition prompt

**工作量**：2-3 天

**风险**：tester 之间的测试可能有重复（同一个 helper 被多 tester 抄一遍） → 第一版接受重复，未来 P2 再做共享 fixture

---

### P1-4 Planner 反并发死锁（孤儿运行）
**维度**: $ / O
**依据**: planner session 2 和 3 在 26 秒内先后启动，session 2 跑完 8 分钟 / 348K tokens 没被使用

**现状**：planner 启动逻辑没有"同 mission 单实例锁"，可能在 UI 双击 / restart_mission 触发时启动两个并发实例。

**提案**：
1. `planner_sessions` 表加 unique partial index：
   ```sql
   CREATE UNIQUE INDEX idx_planner_one_running_per_mission
   ON planner_sessions(mission_id) WHERE status='running';
   ```
2. 启动 planner 时先用 `INSERT OR FAIL`，已存在 running 实例则拒绝
3. UI 上"重新规划"按钮在 mission 有 running planner 时灰显

**验收**：
- 测试：`test_planner_rejects_second_concurrent_session_for_same_mission`

**落点**：`src-tauri/src/db/migrations.rs` (新 migration 加 index) + `src-tauri/src/commands/mission.rs` (plan_mission 提前检测)

**工作量**：0.5 天

**风险**：旧 stale `running` 记录可能永久占锁 → 启动时 timeout 自愈 (running > 30min 视为 stale，自动转 failed)

---

### P1-5 Cost / Token 字段必须实际填充
**维度**: O / $
**依据**: `missions.total_cost_usd = 0.0`、`agents.cost_usd = 0.0`，但 events 里 checkpoint 有 `cost: $0.1086` 字符串

**现状**：cost 在 checkpoint event 的 content 字符串里 (`tokens: 60204in/428out | cost: $0.1230`)，但没回填到 agent / mission 表的数值字段。

**提案**：
1. checkpoint 写入 events 时同时 UPDATE `agents.tokens_used += ?` 和 `agents.cost_usd += ?`
2. agent 完成时触发 mission 级 UPDATE `missions.total_cost_usd = SUM(agents.cost_usd)`
3. UI 上 mission 详情页显示"已花费 $X / 预算 $Y"

**验收**：
- 跑任意 mission，DB 查询 `total_cost_usd` 必须 > 0 且与 checkpoint events 累加值一致 (±2% 误差)
- 测试：`test_checkpoint_event_aggregates_to_cost_fields`

**落点**：`src-tauri/src/agent/engine.rs` (checkpoint handling) + `src-tauri/src/db/queries.rs` (aggregate fn)

**工作量**：1-2 天

**风险**：retroactive 老数据不会回填，UI 上要兼容显示 "—"

---

### P1-6 物理资源治理：worktree 自动清理 + 磁盘可见
**维度**: $ / O
**依据**: 30 个 worktree 实体（15 prunable ref）共 1.0 GB；DB delete agent 时 worktree 不被清

**现状**：`restart_mission(failed_only)` 删 agent record 但 worktree dir 保留（设计如此，怕丢失证据）；prunable ref 也不主动清理。

**提案**：
1. mission `completed` / `failed` 终态超过 N 天（默认 7 天）后自动 `git worktree prune` + 删除 `.worktrees/<orphan>/.build/` 子目录（保留源代码 7 天兜底）
2. UI 上 mission 列表显示磁盘占用列；mission 详情页加"清理 worktree"按钮
3. 设置项加 `max_worktree_disk_gb_per_mission`（默认 2 GB），接近上限时弹警告

**验收**：
- 跑 mission 后 7 天，自动清理任务触发，磁盘占用降到原来的 < 20%
- 测试：`test_worktree_garbage_collection_skips_recent_missions`

**落点**：`src-tauri/src/git/worktree.rs` (gc fn) + 新 background task

**工作量**：2-3 天

**风险**：误删用户想保留的产物 → 加 mission 级 `pin_worktrees` flag 阻止 gc

---

## 4. P2 优化（架构级）

### P2-1 Preflight 收敛信号融入"完成度"门禁
**维度**: C / UX / S
**依据**: convergence=0.64 / 3 个 unfilled slots 时用户强制签约；signed contract 后 4 个核心问题（compaction_summary 列出的）仍悬而未决

**现状**：用户可随时点"签约"，phase 状态只是 UI 提示，不阻塞。

**提案**：
1. 签约按钮根据 `convergence_score` 三态显示：
   - < 0.5：「跳过澄清继续」（红色，需二次确认）
   - 0.5-0.8：「未充分对齐，确认签约」（黄色，弹 modal 列出 unfilled slots）
   - ≥ 0.8：「签约」（绿色）
2. modal 里允许用户对每个 unfilled slot 选 `defer to agent` / `i'll fill in execution` / `skip entirely`
3. 选择被持久化到 `contract_items` 作为 user-source 条款，**让 agent 知道这些是用户授权延后或跳过的，而非遗漏**

**验收**：
- 用例：convergence < 0.5 时点签约，强制弹 modal 列出问题
- 测试：`test_low_convergence_signing_requires_modal_acknowledgement`

**落点**：前端 `src/components/preflight/SignContractButton.tsx` + `src-tauri/src/commands/mission.rs:sign_contract`

**工作量**：5-7 天（涉及 UX 设计）

**风险**：用户烦躁度 ↑ → 必须可"我就要无视，跳过这个弹窗" 一键模式

---

### P2-2 用户参与质量评分 + 反向提醒
**维度**: UX / S
**依据**: 用户在 preflight 最后阶段连续两条「你决定」+ 中午让 agent 自跑没人盯着 + factorial bug 撞了 30 分钟没人介入

**现状**：用户参与质量是隐式的，系统不知道用户什么时候开始疲惫 / 离开。

**提案**：
1. 衡量"用户参与质量"：preflight 最近 5 条 user 消息的平均长度、`你决定`类委托语的出现频率、消息间间隔
2. 当指标退化时（如委托语 > 40%、间隔 > 5min），UI 上 banner 提示「检测到对齐疲劳，建议本轮结束后稍作休息再继续」
3. agent 失败时如果检测到"用户已离开 > 30min"，**自动暂停 mission** 不再 retry，发系统通知（macOS 通知中心）

**验收**：
- 测试：`test_idle_user_detection_pauses_mission_on_failure`

**落点**：前端 monitoring + 后端 `agent/scheduler.rs` (失败时检查 user activity)

**工作量**：1-2 sprint

**风险**：用户感觉"被监视" → 必须可关；策略保守不打扰

---

### P2-3 Agent 工具能力前置探测
**维度**: S / C
**依据**: tester#2 step 40 和 tester#3 step 7 都因 `failed to spawn rg: No such file or directory` 失败——agent 不知道 host 没装 ripgrep

**现状**：tool 在调用时才发现 host 缺失依赖，浪费一步。

**提案**：
1. AgentEngine 启动时探测 host 能力：哪些 CLI 工具可用（rg / ag / jq / curl / swift / cargo）、shell 类型、OS
2. 把可用工具清单作为 system prompt 的固定段落注入：
   ```
   ### Host capabilities
   - search: prefer `grep -r` (rg NOT installed)
   - swift: 5.9 (cd then `swift test`)
   - shell: zsh
   ```
3. 工具调用时若 capabilities 里标了 "NOT installed"，直接拒绝调用（IpcError 友好提示），节省一步

**验收**：
- 测试：`test_agent_system_prompt_includes_host_capabilities`

**落点**：`src-tauri/src/agent/engine.rs` (host probe + prompt) + 启动期 cache

**工作量**：1-2 sprint

**风险**：探测错（用户在 PATH 之外装了 rg） → 加 `override_capabilities` 设置项

---

### P2-4 用户意图溯源：mission Inception → 各执行决策的可追溯链
**维度**: O / UX
**依据**: tester task description 要求"60+ 用例 / 4 个测试文件"，但 contract 17 条 items 里没有任何一条直接对应这个数字——是 planner 自己推导的。用户在 UI 上**无法回溯**"这个数字哪来的"，也无法把后期下发的 FM-06 干预与之关联

**现状**：mission → contract → planner → task → agent → runtime intervention 是多源数据流，但相互之间无显式 provenance 字段。tester 收到"60+ 用例"时，既不知道这是 contract 哪条推导的，也不知道用户后来下发"简化"的 directive 应不应该 override。

**提案**：
1. 每个 task description 字段加一个隐藏的 `provenance` JSON 字段：
   ```json
   {
     "generated_by": "planner_session:f3c70862",
     "based_on_contract_items": ["scope:scientific", "scope:M+", "constraints:test_coverage"],
     "user_review_skipped": true,
     "runtime_directives_applied": ["directive:2026-05-18T06:25 自动化测试可以简化"]
   }
   ```
2. UI 上 task 详情页加"决策溯源"折叠区：展示该 task 内容来自哪些 contract item + 哪些 runtime directive 被 scheduler 拼接到了 prompt
3. 当 user 看"为什么 tester 要写 60+ 用例"时，能点开看到「planner 基于 contract `constraints:test_coverage` 推导，后期收到 `directive:简化测试` 但 agent 实际行为未变（详见 events）」

**验收**：
- UI 集成测试：tester task 详情页能看到"由 planner_session f3c70862 生成，依据 contract item X/Y/Z；后期 runtime directive D1 已注入 prompt"
- 测试：`test_task_description_provenance_chain_complete`
- 测试：`test_provenance_records_runtime_directives_applied_to_retry`

**落点**：Schema 加列 + planner 写入 + UI 展示

**工作量**：1-2 sprint

**风险**：provenance 字段如果不强制 planner 写就是空的 → 必填校验

---

## 5. NO-GO 项（建议不做）

### NO-GO-1 "增大 max_steps 全局上限"
**理由**：单纯放宽 max_steps（从 80 → 150）不能解决根本问题。这次 case 即使给 150 步，tester 仍会撞同样的 bug → 同样修测试绕开 → 仍然失败，只是多烧 ~50% tokens。**真正的修法是 P0-2 (反馈通道) + P1-3 (拆分任务)**。

### NO-GO-2 "把 tester role 移除，让 implementer 自己写测试"
**理由**：会引入更严重的问题：implementer 自测就是"考自己的卷"，对 bug 视而不见。这次 case 里 factorial bug 是 tester 才发现的。tester 作为独立 role 是有价值的，需要的是改 P0-2 让它能反馈，不是删掉。

### NO-GO-3 "用更强模型（Opus）跑 tester"
**理由**：tester#2 用的已经是 deepseek-v4-pro（reasoning model），换 Opus 单次 tokens 成本 × 5-10 倍，对这次 case 失败原因（factorial fatal error）没有任何帮助——强模型也会撞同一个 Swift fatalError。模型选择对的方向是 P2-3 (能力探测) + Sisyphus-takeaways.md 提的智能路由（简单任务用便宜模型），不是无脑 upgrade。

### NO-GO-4 "preflight 强制必须达到 convergence=0.9 才能签约"
**理由**：会让"我就是想快速试一下"的简单 mission 也走完整的 23+ 轮，用户体验毁灭。这次 case 的问题不是"convergence 低不该签"，而是"convergence 低签约后 agent 不知道这件事，仍按 100% 严谨度执行"。修法是 P2-1（低收敛签约时把未决项作为 user-source contract item 持久化，让 agent 知道这些是用户授权延后/跳过的），不是堵住签约。

---

## 6. 优化项一览表（按优先级）

| 编号 | 标题 | 维度 | 工作量 | 落点 |
| --- | --- | --- | --- | --- |
| **P0-1** | Runtime Intervention 升级（hint/redirect/abort + retry 继承 notes + 事务化）| S/C/UX | 5d | commands/agent, scheduler, engine, migrations, UI dialog |
| **P0-2** | Tester 缺陷反馈通道（fixer task） | S/C | 3-5d | tools/definitions, scheduler |
| **P0-3** | Retry 携带前次经验 | S/C/$ | 2-3d | commands/mission, scheduler |
| **P0-4** | wall_clock 按 role 差异化 | S/$ | 1-2d | db/migrations, scheduler, planner_engine |
| **P0-5** | Evaluator 评分按 task scope | O/S | 1d | agent/evaluator |
| **P1-1** | Agent 无效循环熔断 | S/C/$ | 3-5d | scheduler, engine |
| **P1-2** | Evaluator 评分接入 gating | O/S | 5-7d | evaluator, scheduler |
| **P1-3** | Tester 按 module 拆分 | S/C/$ | 2-3d | planner_engine |
| **P1-4** | Planner 同 mission 单实例锁 | $/O | 0.5d | db/migrations, commands/mission |
| **P1-5** | Cost / Token 字段实际填充 | O/$ | 1-2d | agent/engine, db/queries |
| **P1-6** | Worktree 自动 GC | $/O | 2-3d | git/worktree + bg task |
| **P2-1** | 签约门禁感知 convergence | C/UX/S | 5-7d | sign_contract + UI |
| **P2-2** | 用户参与质量评分 + 提醒 | UX/S | 1-2 sprint | UI + scheduler |
| **P2-3** | Agent 工具能力前置探测 | S/C | 1-2 sprint | agent/engine 启动 |
| **P2-4** | 决策溯源链 | O/UX | 1-2 sprint | schema + planner + UI |

---

## 7. 推荐落地节奏

**第 1 周（救急 + 高 ROI 低风险）**：P0-1C（事务化，10 分钟）+ P0-4 + P0-5 + P1-4 + P1-5
→ wall_clock 给够、evaluator 不再错位、planner 不再孤儿、cost 可观察；P0-1C 单独前置因为是个 bug fix 不该等

**第 2 周（核心干预闭环）**：P0-1A + P0-1B + P0-1D
→ 让用户的"中途叫停 + 改道"真正能影响 agent 行为，retry 时干预历史不丢

**第 3-4 周（反馈与 retry 学习）**：P0-2 + P0-3 + P1-1 + P1-3
→ Tester 能向上游报 bug、retry 携带前任经验、agent 早期止损、task 按 module 拆

**第 4-5 周（产品化）**：P1-2 + P1-6
→ Evaluator 真正进入 critical path；磁盘治理常态化

**P2 项**：进入下一个 sprint 规划周期，按 RFC 流程走

---

## 8. 这份提案没回答的问题（留给下次研究）

1. **DAG 边类型语义**：postmortem §4.2 显示 Wave 内有隐式并发限制，但当前 DAG 边只标 `producer/reference`，没有体现"并发组"概念。是否需要引入 `wave` 显式分组？
2. **`merge_strategy='theirs'` 的合理性**：本次因为 implementer 之间没真冲突所以没暴露问题，但 multi-agent 改同文件时 `theirs` 默认让"后完成的赢"未必合理。是否需要 `llm_resolve` 默认化？
3. **Mission 之间的知识复用**：用户做过 1 次 calculator，做第 2 次（不同 mission）时 agent 还是冷启动。是否需要项目级 / 用户级"经验库"？
4. **Preflight mode 选择**：本次用 `risk_highlighter`，但 23 轮的体感更像 `scenario_walk`。三种模式的实际差异和适用边界缺乏数据。
