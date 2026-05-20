# 显式 Merge 节点 方案（一页可评审版）

> 状态：**v1.1 已完成实施**（默认 toggle 关闭，需用户在 Settings 显式启用）
> 范围：DAG 多前序节点的 worktree 合并，从隐式基础设施步骤升级为一等公民节点。
> 关键词：FM-15（Worktree v2.2）、FM-01/07（Planning & DAG）、FM-02（Agent Execution）。

## v1.1 实施落地清单

| 组件 | 状态 | 位置 |
|------|------|------|
| Migration 029 | ✅ | `src-tauri/src/db/migrations.rs`（tasks.kind / merge_parents / missions.verify_command）|
| `NodeKind` 枚举 | ✅ | `src-tauri/src/agent/planner.rs::NodeKind`（serde snake_case，default Work）|
| `PlannerTask.{kind, merge_parents}` | ✅ | 同上；YAML/template 路径默认 Work |
| `inject_merge_nodes` **单节点版**（v1.1 修正） | ✅ | `src-tauri/src/agent/planner_merge_inject.rs`（13 个单测含集成式：N=2..8、validate_task_graph 不退化、闭包语义保持）|
| Inject 接入 planner 流水线 | ✅ | `commands/preflight.rs` + `commands/mission.rs` 在 planner_output 落库前调用 |
| `merge_prompt::build_merge_task_desc` **支持任意 N parents**（v1.1 修正） | ✅ | `src-tauri/src/agent/merge_prompt.rs`（8 个单测：missing JSON / 错误 parent 数 / e2e N=2 + verify+conflicts / no verify+no conflicts / e2e N=4 / N=8 per-parent budget）|
| Scheduler dispatch_task kind 分支 | ✅ | `scheduler.rs::dispatch_task` 接收 `task_kind/merge_parents_json`，merge 节点用专用 task_desc |
| Merge guardrail `CommandPasses` 注入 | ✅ | `scheduler.rs::build_agent_run_options` 对 kind=merge 自动 push CommandPasses，verify_command 优先 mission > AppConfig |
| AppConfig 双开关 | ✅ | `enable_explicit_merge_node`（默认 false）+ `merge_verify_command` |
| Settings UI（Developer 区） | ✅ | `DeveloperSection.tsx` + en/zh i18n |
| TaskDAG merge 节点紫色描边 + ◇ badge | ✅ | `TaskNode.tsx` + `TaskNode.module.css` `data-kind=merge` 选择器 |
| TaskNode tooltip 显示 merge_parents | ✅ | 同上 |
| Mission report `merge_nodes_total` 指标 | ✅ | `report_generator.rs` SELECT COUNT + markdown render |
| 前端 IPC 类型同步 | ✅ | `ipc/commands.ts`（TaskInfo.kind/merge_parents_json + ConfigResponse 字段 + MissionReportMetrics.merge_nodes_total）|

**测试结果**：583 全过（v1 → v1.1 +4 个回归测试覆盖 N=4/N=8/per-parent budget）。
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
| ~~附加~~ | ~~**分层 merge**：N parents → N-1 merge agents，二叉 reduction tree~~ | ~~"可以实现分层 merge"~~ — **v1.1 已撤销，详见下方"分层 merge 撤销记"** |

### 分层 merge 撤销记（v1 → v1.1 设计变更）

**用户在 v1 验收时挑战**："N=8 parents 时会插 7 个 merge node，DAG 视觉膨胀，这是为什么？不可以一个 merge node 在 child 节点的前序吗？"

**复盘**：v1 选二叉 reduction tree 是误判。把 MPI_Reduce / parallel sort 的"分层并行合并"经验直接搬来了，但 merge agent 场景**没有适用前提**：

| 维度 | 二叉 reduction tree | 单 merge node 合 N | 谁赢 |
|------|---------------------|---------------------|------|
| 时延 | 深度 `ceil(log2(N))` 层**串行** LLM session | 单 LLM session | 单 node |
| Token 总成本 | N-1 个 agent 各自 system prompt + 工具表 overhead | 单 agent 一次性看完 | 单 node 通常更省 |
| DAG 视觉 | N=8 → 7 个紫色 ◇ 节点 | 1 个 ◇ 节点 | 单 node 完胜 |
| 调试 | 用户跨 N-1 个 sub-merge timeline 跳转 | 一个 timeline 看完整推理 | 单 node |
| 失败放大 | leaf merge 失败 → 上层全白做，重试从 leaf 开始 | 失败一次，重试一次 | 持平 |
| 用户原话 | "显式的节点"（**单数**） | 字面对齐 | 单 node |
| **二叉树唯一**边际价值 | 单 merge agent 上下文短（只看 2 个 parent diff） | N 大时 prompt 大 | 仅 N≥10 有意义 |

reduction tree 的核心理论卖点是**并行性**（workers 同时合各对），但 merge agent 必须等 parents 全部 completed 才能跑（DAG 依赖），单 agent 内部又是串行——**完全没有并行收益**。剩下的"上下文小"在 200K-context 模型时代基本不值钱（N=4 的 4 路 diff 远不到上限）。

**v1.1 决策**：单 merge node 合 N parents。`inject_merge_nodes` 对每个多 parent 任务**恰好**插 1 个 merge node，其 `depends_on = merge_parents = parents 全集`。

**未来可选优化**：如实测发现 N≥10 时单 merge agent 的 prompt 超出 token budget，可加 fallback：`N <= threshold` 用单 merge，`N > threshold` 退化为 reduction tree。当前无证据支持该 threshold，先不实现。

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

## 2. 关键决策（已拍板）

详见上文"决策记录"。本节为历史候选对照表，便于回溯设计动机。

| # | 决策 | 选项 | 最终选择 |
|---|------|------|----------|
| **D1** | **节点形态** | A 新 `NodeKind::Merge` / B 复用 task + 新 role / C 不动 DAG 拆 sub-agent | **A**（最符合用户原话"显式的节点"） |
| **D2** | **Merge 策略** | A git theirs+LLM 后处理 / B LLM 语义级 merge / C hunk 级 LLM | **复用通用 AgentEngine**（用户拍板：让 merge agent 拿通用 tools 自己解，不做策略选择） |
| **D3** | **Quality gate** | A 硬性 build/lint/test / B LLM self-review+人审 / C 可配置 | **in-agent verify_command + guardrail**（既不挂人也不一刀切） |
| **D4** | **失败兜底** | A 自动重试 N=2 失败挂人 / B 一失败就挂人 / C 降级 theirs 标 needs_review | **不挂人**（用户拍板：失败显式 fail，final message 即错因） |

---

## 3. 整体方案（已拍板版本）

### 3.1 拓扑：N parents → **1 个** merge node（v1.1）

```
N=2 (diamond):
  A ──┐
      ├──► merge-X ──► X
  B ──┘

N=4:
  A ──┐
  B ──┤
  C ──┼──► merge-X ──► X
  D ──┘

N=8:
  P1..P8 ──► merge-X ──► X     (1 merge node, depends_on=[P1..P8])
```

每个汇合点**恰好** 1 个 merge node：
- `merge_node.depends_on = merge_node.merge_parents = X 的原 parents 全集`（顺序保持稳定）
- `X.depends_on = [merge_node.id]`（X 现在只依赖单一 merge）
- DAG 视觉干净，UI 不爆炸

### 3.2 关键不变量：**"merge agent 就是普通 agent"**

- 跑的是同一个 `AgentEngine`，同一套 tools (`read_file` / `write_file` / `shell_exec` / `task_complete`)、同一套 P0/P1/P2 能力（reactive compact / token budget / withhold-recover / fallback / hooks）
- 调度路径：`scheduler::dispatch_task` 看到 `task.kind = "merge"` → 仍走 `AgentEngine::run`，差异**只在 system prompt 模板**和 **task_complete guardrail**
- 工作环境准备 = 已有的 `prepare_task_base` + theirs 兜底：merge agent 看到的初始 worktree 就是"已经 ref-only merge 完 N 个 parent、有冲突文件标 theirs"的起点，**它的工作 = 把这个起点变成 verified-clean**
- worktree commit 仍是 `agent/<merge_agent_uuid>`，下游 task 把它当**唯一直接 parent** 拉，递归走通

### 3.3 核心组件

| 组件 | 文件 | 职责 | 工作量 |
|------|------|------|--------|
| **`tasks.kind` + `tasks.merge_parents`** | DB migration 029 | `kind TEXT NOT NULL DEFAULT 'work' CHECK IN ('work','merge')` + `merge_parents TEXT`（JSON 数组） | 小 |
| **`NodeKind` 枚举** | `src-tauri/src/agent/planner.rs` | `enum NodeKind { Work, Merge }`；`PlannerTask` 加 `kind: NodeKind` + `merge_parents: Vec<String>` | 小 |
| **Planner 后处理（单节点版）** | `src-tauri/src/agent/planner_merge_inject.rs` | 跑完 validate 后扫描：≥2 parent → 插入 1 个 merge node；原下游 task.depends_on = [merge_node.id] | 小（v1.1 简化后） |
| **Merge prompt 模板** | `src-tauri/src/agent/merge_prompt.rs` | 注入：N 个 parent 的 task 描述、目标、agent 分支名（让 agent 自己 `git diff`）、冲突文件清单（按 parent 分组）、verify_command、"成功标准 = verify 通过 + task_complete" | 中 |
| **Scheduler dispatch 分支** | `src-tauri/src/agent/scheduler.rs::dispatch_task` | `if task.kind == Merge` → 用 merge prompt 起 AgentEngine，其余路径不变 | 小 |
| **Merge guardrail** | `scheduler.rs::build_agent_run_options` 注入 `CommandPasses` | `task_complete` 触发时校验 verify_command 至少跑过一次且 exit=0；失败 → 注入 system_hint 让 agent 修复 | 小 |
| **UI 渲染** | `src/components/mission/TaskNode.tsx` + `.module.css` | merge 节点紫色描边 + ◇ badge；tooltip 显示 N 个 parent | 中 |
| **Report** | `src-tauri/src/agent/report_generator.rs` | `ReportMetrics.merge_nodes_total` 指标 + markdown "Merge nodes" 行 | 小 |
| **Settings toggle** | `AppConfig.enable_explicit_merge_node` 默认 false（v1 灰度） + Settings UI | 关 → 走老 `prepare_task_base` 隐式路径，行为字节对等 | 小 |

### 3.4 失败语义（关键）

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
| R1 | 性能 | 每个汇合点多 1 个 agent，token / 时长成本涨 | v1.1 已从 N-1 个 agent 降到 1 个 agent，开销 = 1 个普通 task 量级 |
| R2 | 兼容 | 老 mission（migration 前）DB 没有 `tasks.kind` 列 | migration 加列 + default `work`；planner 仅对启用 toggle 的新 mission 注入；toggle 默认 false |
| R3 | 准确性 | merge agent 偶发解错 → 下游 task 在错代码起步 | guardrail 强制 verify_command 通过才能 task_complete；下游 task 仍会发现并 fail |
| R4 | UI 复杂度 | DAG 节点数 +1 每汇合 | v1.1 已无视觉膨胀；merge 节点紫色 ◇ badge 一眼可辨 |
| R5 | 失败 | merge agent 失败 → mission failed → 用户 frustration | 错误信息明确（agent 自己写的 final message）+ 提供"从这个 merge 节点重跑"按钮（复用 FM-08 task 重启） |
| R6 | 死循环 | merge agent 反复改 - verify - 失败 - 改 → 耗光 budget | P0-2 token budget 已覆盖；耗光后给明确 final message |
| R7 | **N 过大** | N≥10 时单 merge agent 的 prompt 可能超出 context budget（未观察到） | 当前 per-parent description 动态分配 char budget（min 300）；如实测溢出可加 fallback 退化为 reduction tree |
| V1 | 验证完成 | merge agent 在两个 parent 改同文件不同行（非冲突 hunk）时能否正确认知 | v1.1 实测 + 单测覆盖 N=2..8 |

---

## 5. 里程碑（已完成）

| M | 状态 | 交付物 |
|---|------|--------|
| M1 (v1) | ✅ 已完成并 push | DB migration 029 + `NodeKind` + `inject_merge_nodes`（初版二叉 tree）+ merge_prompt + scheduler dispatch + guardrail |
| M2 (v1) | ✅ 已完成并 push | AppConfig 双开关 + Settings UI + 默认 false 灰度 |
| M3 (v1) | ✅ 已完成并 push | TaskDAG ◇ badge + Mission report merge_nodes_total + i18n |
| **M4 (v1.1)** | ✅ **本次提交** | **撤销二叉 reduction tree，改为单 merge node per join**；prompt 支持任意 N parents；测试覆盖 N=2..8 |

---

## 附录 A：与"隐式 merge"的 byte-level 兼容路径

`enable_explicit_merge_node=false`（默认）→ 走老 `prepare_task_base` 隐式路径，行为 100% 字节对等，所有现有测试不变。

`enable_explicit_merge_node=true`（用户主动开）→ planner 给新 mission 注入 merge node；老 mission（无 `tasks.kind` 列、planner_version < 2.3）继续走旧路径不回溯改写。

## 附录 B：v1.1 算法（单 merge per join）

```rust
fn inject_merge_nodes(tasks: &mut Vec<PlannerTask>, opts: InjectOptions) -> usize {
    if !opts.enabled { return 0; }
    let plan: Vec<(usize, Vec<String>)> = tasks.iter().enumerate()
        .filter_map(|(idx, t)| {
            if t.kind == NodeKind::Merge { None }
            else if t.depends_on.len() >= 2 { Some((idx, t.depends_on.clone())) }
            else { None }
        })
        .collect();

    let mut new_nodes = Vec::new();
    for (task_idx, parents) in plan {
        let downstream_id = tasks[task_idx].id.clone();
        let merge_id = format!("merge-{downstream_id}");
        new_nodes.push(PlannerTask {
            id: merge_id.clone(),
            kind: NodeKind::Merge,
            depends_on: parents.clone(),
            merge_parents: parents,
            // ... title/description ...
        });
        tasks[task_idx].depends_on = vec![merge_id];
    }
    let added = new_nodes.len();
    tasks.extend(new_nodes);
    added
}
```

复杂度：N parents → **1 个** merge node，深度 1（不分层）。`O(tasks)` 时间，`O(汇合点数)` 空间。
