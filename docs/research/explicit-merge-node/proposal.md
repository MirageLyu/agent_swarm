# 显式 Merge 节点 方案（一页可评审版）

> 状态：**v1 已完成实施**（默认 toggle 关闭，需用户在 Settings 显式启用）
> 范围：DAG 多前序节点的 worktree 合并，从隐式基础设施步骤升级为一等公民节点。
> 关键词：FM-15（Worktree v2.2）、FM-01/07（Planning & DAG）、FM-02（Agent Execution）。

## v1 实施落地清单

| 组件 | 状态 | 位置 |
|------|------|------|
| Migration 029 | ✅ | `src-tauri/src/db/migrations.rs`（tasks.kind / merge_parents / missions.verify_command）|
| `NodeKind` 枚举 | ✅ | `src-tauri/src/agent/planner.rs::NodeKind`（serde snake_case，default Work）|
| `PlannerTask.{kind, merge_parents}` | ✅ | 同上；YAML/template 路径默认 Work |
| `inject_merge_nodes` 二叉 reduction tree | ✅ | `src-tauri/src/agent/planner_merge_inject.rs`（11 个单测含集成式：N=1..6、validate_task_graph 不退化、闭包语义保持）|
| Inject 接入 planner 流水线 | ✅ | `commands/preflight.rs` + `commands/mission.rs` 在 planner_output 落库前调用 |
| `merge_prompt::build_merge_task_desc` | ✅ | `src-tauri/src/agent/merge_prompt.rs`（4 个单测：missing JSON / 错误 parent 数 / e2e with verify+conflicts / no verify+no conflicts）|
| Scheduler dispatch_task kind 分支 | ✅ | `scheduler.rs::dispatch_task` 接收 `task_kind/merge_parents_json`，merge 节点用专用 task_desc |
| Merge guardrail `CommandPasses` 注入 | ✅ | `scheduler.rs::build_agent_run_options` 对 kind=merge 自动 push CommandPasses，verify_command 优先 mission > AppConfig |
| AppConfig 双开关 | ✅ | `enable_explicit_merge_node`（默认 false）+ `merge_verify_command` |
| Settings UI（Developer 区） | ✅ | `DeveloperSection.tsx` + en/zh i18n |
| TaskDAG merge 节点紫色描边 + ◇ badge | ✅ | `TaskNode.tsx` + `TaskNode.module.css` `data-kind=merge` 选择器 |
| TaskNode tooltip 显示 merge_parents | ✅ | 同上 |
| Mission report `merge_nodes_total` 指标 | ✅ | `report_generator.rs` SELECT COUNT + markdown render |
| 前端 IPC 类型同步 | ✅ | `ipc/commands.ts`（TaskInfo.kind/merge_parents_json + ConfigResponse 字段 + MissionReportMetrics.merge_nodes_total）|

**测试结果**：577 → 579 全过（+2 集成式断言，+15 本 v1 累计新增）。
**前端 build**：通过。
**默认行为**：toggle 关闭时与旧版字节对等，所有现有 mission 不受影响。

## 决策记录（用户拍板）

| # | 选择 | 用户原话 |
|---|------|----------|
| D1 | **A** 新 `NodeKind::Merge` | "显式的节点来做 merge" |
| D2 | **复用通用 AgentEngine**（不是 hunk 级 LLM 也不是脚本化 merge）—— merge agent 拿 read/write/shell_exec 等通用 tools，自己看 base 起点 + 冲突清单，自己决定怎么解 | "要有和通用 agent 相当的能力，复用同一套 agent harness" |
| D3 | Verify gate **存在但 in-agent**：merge agent 自己跑 `shell_exec` 调 verify_command；scheduler 在 task_complete 时通过 guardrail 验证 verify_command 至少跑过一次且最后一次 exit=0 | "和其它 agent 执行节点一样" |
| D4 | **不挂人**：失败直接 mission failed，merge agent 的 final message 即错误说明 | "如果失败也不要人兜底，由 LLM 给出明确的错误信息" |
| 附加 | **主 model**：不加 `agent_merge_model` 配置 | "Merge 节点用主 model" |
| 附加 | **分层 merge** 进 v1：N parents → N-1 merge agents，二叉 reduction tree，深度 ceil(log2(N)) | "可以实现分层 merge" |

---

## 1. 问题与目标

**现状**：当一个 DAG task 有多个直接 parent（菱形汇合），`Scheduler::dispatch_task` 会**隐式**调 `WorktreeManager::prepare_task_base`：
- 从 main HEAD 新建 `task-base/<task_id>` 分支
- 按 `agent/<parent_uuid>` 顺序逐个 **ref-only `merge_trees`** 合入
- 冲突 → **强制 theirs 兜底**（最后 merge 的 parent 赢）+ 写 `task_base_conflicts` 表 + 通过 codebase_intel 注入 `[Base Conflicts]` 提示
- **task-base 阶段不调 LLM**；LLM 只在 mission 终态合 main 时跑（且仅 `merge_strategy=llm_resolve`）
- **无 build / lint / test gate**，无失败回滚

**痛点**：
1. **合并质量黑盒**：parent A 改 `auth.ts`、parent B 也改 `auth.ts` → 后到的赢，前面的工作可能被静默吃掉
2. **下游 agent 替合并兜底**：被注入"这里有 base 冲突，你看着办"——但下游 agent 上下文是去做 task X，不是去做语义合并，分心 + 容易漏处理
3. **失败模式不可见**：合并出问题不会让 task fail，只是默默拼出一个错的起点，下游再花 5 个 step 才发现
4. **顺序敏感**：同深度 parent 按 `completed_at` 排序——并发跑出来的顺序就是合并优先级，毫无语义性
5. **没有质量门**：合并完不跑 build，下游 agent 第一步 `tsc` 才发现根本编译不过

**v1 目标**：让"merge"成为 DAG 一等公民节点，拥有独立 LLM 上下文（看到各 parent 的目标 / diff / 冲突）+ 显式质量 gate（build/test 通过才放行）+ 失败可见可干预。

**v1 明确不做**：
- 不替代终态 main merge（`merge_completed_mission` 路径不动）
- 不支持"自定义 merge 算法插件"（先内置一两种策略）
- 不支持 merge 节点本身递归依赖另一个 merge 节点的差异化策略（用相同算法即可）
- 不做"merge 节点跨 mission 复用 / 缓存"（合一次跑一次）
- 不动 `task_base_conflicts` 表 schema（兼容旧路径）

---

## 2. 关键决策（**等用户拍板的 4 个**）

| # | 决策 | 候选 A | 候选 B | 候选 C | 默认偏好 |
|---|------|--------|--------|--------|----------|
| **D1** | **节点形态** | **新 `NodeKind::Merge`**，planner 在多 parent 汇合处自动插入"合并节点"；老 task 仅做单一职责 | 复用现有 task + 新 `MergeAgent` role；scheduler 看到 role=merge 时走特殊路径 | 不动 DAG，把 `prepare_task_base` 内部拆成"sub-agent"（独立 LLM 上下文但 DAG 不可见） | **A**（让 merge 在 UI / report / DAG 图里显式可见，最符合用户原话"显式的节点"） |
| **D2** | **Merge 策略** | git 3-way merge 兜底 theirs（同现状）+ **LLM 后处理冲突文件**（最贴近现 mission 终态那条路） | LLM **语义级 merge**：把每个 parent diff 作为"意图描述"交给 LLM，让它从零融合（最激进，token 贵） | git 3-way merge → **每个冲突 hunk** 单独给 LLM（最细粒度，复用 `conflict_resolver.rs` 的 `LlmProviderResolver`） | **C**（hunk 级最经济、复用度最高；现 `conflict_resolver.rs` 已是这套抽象） |
| **D3** | **Quality gate** | 硬性 build/lint/test 必须过（跑 mission 配置的 `verify_command`，无则用默认 `cargo check` / `npm run build`） | LLM self-review（合完让 reviewer-agent 看一遍）+ 进 Approval Queue 等人审 | **可配置**：默认硬 gate；mission 未配置 verify_command 时 fallback 到 LLM self-review；用户可在 Settings 选"严格 / 宽松 / 关闭" | **C**（避免一刀切；零配置项目能用 LLM self-review，配置过 verify 的走硬 gate） |
| **D4** | **失败兜底** | 自动重试 N=2 次（不同 prompt 策略：第 1 次保守、第 2 次激进 ours/theirs 偏向），仍失败 → **挂起** Approval Queue | 一失败就挂 Approval Queue，等人解决冲突 / 决定继续 / 取消 mission（人在 loop） | 降级到当前 theirs 兜底行为，但 merge 节点状态标 `needs_review` 让 DAG 继续；用户在 mission report 看到 warning | **A**（先自动重试给机会自愈；最终人兜底；不静默降级——降级是当前痛点） |

> **注**：每个 D 的"默认偏好"是我给你的推荐答案；你直接说 "D1=A, D2=C, D3=C, D4=A" 即可开工，或者改任意一个。

---

## 3. 整体方案（已拍板版本）

### 3.1 拓扑：N parents → 二叉 reduction tree

N=2 平凡，菱形 + 高 fan-in 示例：

```
N=2:
  A ──┐
      ├──► M(A,B) ──► 下游 D
  B ──┘

N=4 (二叉 reduction tree, 深度 2):
  A ──┐
      ├──► M1(A,B) ──┐
  B ──┘              ├──► M3(M1,M2) ──► 下游 D
  C ──┐              │
      ├──► M2(C,D) ──┘
  D ──┘

N=5 (奇数: M1=(A,B), M2=(C,D), M3=(M1,M2), M4=(M3,E)):
  A,B → M1 ┐
           ├─ M3 ┐
  C,D → M2 ┘    ├─ M4 ──► 下游 D
  E ───────────-┘
```

每个 merge agent 永远只看 **2 个 parent**，token 上下文小、可调试。

### 3.2 关键不变量：**"merge agent 就是普通 agent"**

- 跑的是同一个 `AgentEngine`，同一套 tools (`read_file` / `write_file` / `shell_exec` / `task_complete`)、同一套 P0/P1/P2 能力（reactive compact / token budget / withhold-recover / fallback / hooks）
- 调度路径：`scheduler::dispatch_task` 看到 `task.kind = "merge"` → 仍走 `AgentEngine::run`，差异**只在 system prompt 模板**和 **task_complete guardrail**
- 工作环境准备 = 已有的 `prepare_task_base` + theirs 兜底：merge agent 看到的初始 worktree 就是"已经 ref-only merge 完、有冲突文件标 theirs"的起点，**它的工作 = 把这个起点变成 verified-clean**
- worktree commit 仍是 `agent/<merge_agent_uuid>`，下游 task 把它当**唯一直接 parent** 拉，递归走通

### 3.3 核心组件

| 组件 | 文件 | 职责 | 工作量 |
|------|------|------|--------|
| **`tasks.kind`** | DB migration 029 | `kind TEXT NOT NULL DEFAULT 'work' CHECK IN ('work','merge')` + `tasks.merge_parents JSON` 存合并对 | 小 |
| **`NodeKind` 枚举** | `src-tauri/src/agent/planner.rs` | `enum NodeKind { Work, Merge }`；`PlannerTask` 加 `kind: NodeKind` + `merge_parents: Vec<String>` | 小 |
| **Planner 后处理** | `src-tauri/src/agent/planner_state.rs`（新增 `inject_merge_nodes` 函数） | 跑完 validate 后扫描：≥2 parent → 用二叉 reduction 算法插入 N-1 个 merge node；原下游 task.depends_on = [最后一个 merge node] | 中 |
| **Merge prompt 模板** | 新建 `src-tauri/src/agent/prompts/merge_agent.md` + `src-tauri/src/agent/prompt_builder.rs` 加分支 | 注入：两个 parent 的 task 描述、目标、diff summary（`git diff parent1..parent2`）、冲突文件清单、verify_command、"成功标准 = verify 通过 + task_complete" | 中 |
| **Scheduler dispatch 分支** | `src-tauri/src/agent/scheduler.rs::dispatch_task` | `if task.kind == Merge` → 用 merge prompt 起 AgentEngine，其余路径不变 | 小 |
| **Merge guardrail** | 新建 `src-tauri/src/agent/merge_guardrail.rs`（或扩 `run_guardrails`） | `task_complete` 触发时校验：①worktree 干净（无未 commit 改动 or 自动 commit 完）②本次 session 至少跑过一次 verify_command 且最近一次 exit=0；失败 → 注入 system_hint 让 agent 重新跑 verify 或修复 | 中 |
| **UI 渲染** | `src/components/mission/TaskDAG.tsx` | merge 节点菱形 icon + 紫色描边；hover 看 parent 列表；click 进 workspace 看 agent 完整 timeline（复用已有 view） | 中 |
| **Report** | `src-tauri/src/agent/report_generator.rs` | "Merge events" 段：每个 merge agent 的 parents / step 数 / verify 跑了几次 / token | 小 |
| **Settings toggle** | `AppConfig.enable_explicit_merge_node` 默认 false（v1 灰度） + Settings UI | 关 → 走老 `prepare_task_base` 隐式路径，行为字节对等 | 小 |

### 3.4 失败语义（**关键修订**）

- merge agent 自己跑：read 冲突文件 → 修 → `shell_exec` verify_command → 如果失败再修 → 再 verify → 直到 ok 调 `task_complete`
- 出口 1（成功）：guardrail 通过 → task completed → 下游可以跑
- 出口 2（agent 自己放弃）：agent 调 `task_failed` 工具（已有）→ task failed → mission failed，**agent 的 final message 直接作为 mission failure reason 展示**（不再二次包装）
- 出口 3（资源耗尽）：超 max_steps / 超 token budget / 超 max_retries → engine 标 failed，complaint message 同样作为 mission failure reason
- **不进 Approval Queue**，**不 fallback theirs**——失败就显式失败，让用户去看 merge agent 的 timeline 自己判断

### 3.5 和已有功能的关系

- **P0-2 Token Budget**：merge agent 用全局 `agent_output_token_budget`；budget 耗尽 → 像普通 agent 一样停（不会卡 mission）
- **P1-2 Cross-Model Fallback**：merge agent 复用主 model，遇 5xx 自动 fallback（如果配了）
- **P2-1 Hooks**：merge agent 是普通 agent，所有 hook phase 适用；用户可以注册 PostToolUse hook 强制每次写文件后跑 `tsc --noEmit`
- **FM-14 Approval**：**不接入**（决策 D4）
- **FM-15 旧路径**：toggle 关时 100% 不变；toggle 开时仍调 `prepare_task_base` 准备初始 worktree（提供 theirs 兜底起点），但下游 task 看到的是 merge agent 的 `agent/<uuid>` 而非 `task-base/<id>`

---

## 4. 风险与待验证项

| # | 类型 | 描述 | 缓解 |
|---|------|------|------|
| R1 | 性能 | 每个汇合点多 N-1 个 agent，token / 时长成本涨 | 二叉 reduction tree 让单 agent 上下文小；merge agent 在简单 case 通常 2-3 step 收敛 |
| R2 | 兼容 | 老 mission（migration 前）DB 没有 `tasks.kind` 列 | migration 加列 + default `work`；planner 仅对 v2.3+ 注入；toggle 默认 false |
| R3 | 准确性 | merge agent 偶发解错 → 下游 task 在错代码起步 | guardrail 强制 verify_command 通过才能 task_complete；下游 task 仍会发现并 fail |
| R4 | UI 复杂度 | DAG 节点数翻倍 | merge 节点用菱形 icon + 紧凑样式；默认折叠 reduction tree（点开看分层） |
| R5 | 链式失败 | 一个 merge agent 失败 → mission failed → 用户 frustration | 错误信息明确（agent 自己写的 final message）+ 提供"从这个 merge 节点重跑"按钮（复用 FM-08 task 重启） |
| R6 | 死循环 | merge agent 反复改 - verify - 失败 - 改 → 耗光 budget | P0-2 token budget 已覆盖；耗光后给明确 final message |
| **V1** | 待验证 | merge agent 在两个 parent 改同文件不同行（非冲突 hunk）时能否正确认知 | M1 spike：跑 1 个真实菱形 case 看 timeline |
| **V2** | 待验证 | verify_command 在大仓库的耗时 | 用户已有 mission verify_command，按现状跑；超时由 P0-2 budget 兜底 |

---

## 5. 里程碑（**已拍板版本**，跳过原 D2 spike）

> 用户判定 "merge 对模型不是问题"，跳过原 M1 的 D2 选型 spike；改为 M1 = **最小可跑通**（手工触发 1 个菱形 case 验证 prompt 模板和 guardrail 协作），过了直接展开 M2-M5。

| M | 时长 | 交付物 | 验证标准 |
|---|------|--------|----------|
| **M1** | 2-3 天 | **最小可跑通**：DB migration 029 + `NodeKind` 枚举 + `inject_merge_nodes` 算法（含分层 reduction tree）+ `merge_agent.md` prompt 模板 + scheduler dispatch 分支；手工构造 1 个 4-parent mission 跑通 | 单测：planner 注入 4 parent → 3 merge node 拓扑正确；手动 e2e：mission 跑完 worktree 无冲突标记 + verify 通过 |
| **M2** | 2 天 | **Merge guardrail**：task_complete 时校验 verify_command 跑过 + 最近一次 exit=0；失败注入 system_hint 让 agent 继续；超 retry → 自然 fail | 集成测试：故意构造 verify 一直失败的 mock case，验证 mission fail + final message 含错误说明 |
| **M3** | 2 天 | **UI**：TaskDAG merge 节点菱形样式 + reduction tree 折叠 + Settings toggle + Mission report Merge events 段 + i18n（en/zh） | 跑一个真实菱形 mission，截图 DAG / report；Settings toggle 关掉行为字节对等 |
| **M4** | 1 天 | **文档 + 灰度**：FM-15 SR 增补 + 用户迁移指南 + 默认 false 提交 | proposal.md status → "Done"；CHANGELOG 写法 |

**总计 7-8 天**（约 1.5 周）。

---

## 附录 A：与"隐式 merge"的 byte-level 兼容路径

`enable_explicit_merge_node=false`（默认）→ 走老 `prepare_task_base` 隐式路径，行为 100% 字节对等，所有现有测试不变。

`enable_explicit_merge_node=true`（用户主动开）→ planner 给新 mission 注入 merge node；老 mission（无 `tasks.kind` 列、planner_version < 2.3）继续走旧路径不回溯改写。

## 附录 B：reduction tree 算法（简明伪码）

```rust
fn inject_merge_nodes(task: &mut PlannerTask, parents: Vec<TaskId>) -> Vec<PlannerTask> {
    if parents.len() < 2 { return vec![]; }
    let mut layer: VecDeque<TaskId> = parents.into();
    let mut new_nodes = vec![];
    while layer.len() > 1 {
        let mut next_layer = VecDeque::new();
        while layer.len() >= 2 {
            let p1 = layer.pop_front().unwrap();
            let p2 = layer.pop_front().unwrap();
            let merge_id = format!("M-{}", uuid::new_v4());
            new_nodes.push(PlannerTask {
                id: merge_id.clone(),
                kind: NodeKind::Merge,
                merge_parents: vec![p1.clone(), p2.clone()],
                depends_on: vec![p1, p2],
                title: format!("Merge: {} + {}", p1_title, p2_title),
                ..default()
            });
            next_layer.push_back(merge_id);
        }
        // 奇数余下 1 个 → 进下一层和上一层产物配对
        if let Some(odd) = layer.pop_front() {
            next_layer.push_back(odd);
        }
        layer = next_layer;
    }
    task.depends_on = vec![layer.pop_front().unwrap()];  // 唯一 root merge
    new_nodes
}
```

复杂度：N parents → N-1 merge nodes，深度 ceil(log2(N))。N=5 时 4 个 merge node、深度 3。
